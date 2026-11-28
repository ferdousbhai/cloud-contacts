//! The main window: every linked account's contacts in a searchable sidebar
//! (sorted by last name; an account filter above it when there is more than
//! one), the chosen one's card on the right with the account it is in, and
//! one dialog to add or edit a contact. Phone numbers copy on a click, emails
//! and links open. The accounts menu links an iCloud or Google account, signs
//! in again or unlinks one. Fetches every account on opening and every
//! [`FETCH_SECS`] while shown.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::sync::Arc;

use gtk::prelude::*;
use gtk::{gio, glib};
use omui::icons::name;
use omui::{background, i18n, tr, trn};

use cloud_contacts::accounts;
use cloud_contacts::book::{Book, Entry, Shown};
use cloud_contacts::config;
use cloud_contacts::dedupe;
use cloud_contacts::draft::{Draft, Problem, Row, SCALARS, typed};
use cloud_contacts::error::Error;
use cloud_contacts::icloud::api::SessionTransport;
use cloud_contacts::mirror;
use cloud_contacts::model::{self, Field, Multi};
use cloud_contacts::store::Store;
use icloud_app_ui::SignInBanner;

const FETCH_SECS: u32 = 300;

pub struct Window {
    window: gtk::ApplicationWindow,
    shell: Rc<omui::Shell>,
    banner: SignInBanner,
    search: gtk::SearchEntry,
    list: gtk::ListBox,
    /// What the sidebar shows in place of the contacts when it has none to show.
    list_empty: omui::EmptyState,
    card: gtk::Box,
    card_stack: gtk::Stack,
    split: omui::Split,
    edit_button: gtk::Button,
    delete_button: gtk::Button,
    /// Which account the sidebar shows: "All Accounts", then each by name.
    filter: gtk::DropDown,
    filter_names: gtk::StringList,
    /// The accounts menu's button, given a new menu for each change of the accounts.
    accounts_button: gtk::MenuButton,
    stack: gtk::Stack,
    transport: Arc<SessionTransport>,
    root: Option<PathBuf>,
    /// The linked accounts' names and providers, as last read.
    accounts: RefCell<Vec<(String, String)>>,
    shown: RefCell<Shown>,
    /// The chosen contact's id; empty for none.
    selected: RefCell<String>,
    /// The ids the search matches; `None` without a search.
    visible: RefCell<Option<HashSet<String>>>,
    syncing: Cell<bool>,
    last_error: RefCell<Option<String>>,
    /// Mirroring iCloud and Google: the offer above the contacts,
    /// and the switch with its status line below the sidebar. Neither shows
    /// without both an iCloud and a Google account.
    ks_offer: omui::Banner,
    ks_list: gtk::ListBox,
    ks_row: omui::Row,
    ks_switch: gtk::Switch,
    /// Where the window last put the switch: only a move away from it is the person's. A flag held up while the
    /// window moves the switch was not enough: the move back to off that turning on makes, from inside the switch's
    /// own handler, was taken for the person's, and turning on also turned off.
    ks_shown: Cell<bool>,
    /// "N contacts look like duplicates", over the contacts, while any do.
    dup_offer: omui::Banner,
    /// While the add or edit dialog holds what was typed: whether that differs from the contact as it was loaded.
    /// Closing the window asks before it loses such work.
    editing: RefCell<Option<Rc<dyn Fn() -> bool>>>,
    /// Whether the question to discard what was typed is on screen: it is asked once, not again over itself.
    asking: Cell<bool>,
    /// The sidebar's menu, written again in a new language.
    main_menu: gio::Menu,
    /// What the card pane shows with no contact chosen.
    no_card: omui::EmptyState,
    /// The overlay listing every shortcut, on F1 and Ctrl+?.
    shortcuts: omui::ShortcutOverlay,
    /// The sidebar's title, the mirroring switch's title, and the mirror line under it as last shown, kept as what it
    /// says so that a switch of language writes it again.
    mirror_line: RefCell<Option<mirror::Line>>,
    /// How many contacts look like duplicates, for the banner's line.
    duplicates: Cell<usize>,
}

impl Window {
    pub fn new(app: &gtk::Application) -> Rc<Self> {
        let this = Rc::new_cyclic(|weak: &Weak<Window>| {
            let (w1, w2) = (weak.clone(), weak.clone());
            // The banner keeps its text from here on (icloud-app-ui takes it once): it is in the language the window
            // opened in.
            let ask: &'static str = Box::leak(tr("Sign in to iCloud to see your iCloud contacts").into_boxed_str());
            let banner = SignInBanner::new(
                ask,
                move || {
                    if let Some(this) = w1.upgrade() {
                        this.sync();
                    }
                },
                move |msg| {
                    if let Some(this) = w2.upgrade() {
                        this.toast(&msg);
                    }
                },
            );
            build(app, banner)
        });
        let keep_alive = RefCell::new(Some(this.clone()));
        let weak = Rc::downgrade(&this);
        this.window.connect_close_request(move |_| {
            // The compositor can close the window over an open dialog, which the window's own keys cannot: what
            // was typed there is asked about first, and the window stays unless the answer is to discard it.
            if let Some(this) = weak.upgrade() {
                if this.asking.get() {
                    return glib::Propagation::Stop;
                }
                let unsaved = this.editing.borrow().clone();
                if unsaved.is_some_and(|unsaved| unsaved()) {
                    this.ask_discard(
                        |_| {},
                        |this| {
                            this.editing.take();
                            this.window.close();
                        },
                    );
                    return glib::Propagation::Stop;
                }
            }
            keep_alive.borrow_mut().take();
            glib::Propagation::Proceed
        });
        this.connect();
        this.install_keys();
        let weak = Rc::downgrade(&this);
        i18n::on_change(&this.window, move || {
            if let Some(this) = weak.upgrade() {
                this.retranslate();
            }
        });
        this.load_cached();
        this.sync();
        let weak = Rc::downgrade(&this);
        glib::timeout_add_seconds_local(FETCH_SECS, move || {
            let Some(this) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if this.window.is_visible() && !this.window.is_suspended() {
                this.sync();
            }
            glib::ControlFlow::Continue
        });
        this
    }

    pub fn present(&self) {
        self.window.present();
        // Typing a name filters the list at once; GTK's own first focus
        // would be the header's menu button.
        self.search.grab_focus();
    }

    fn toast(&self, msg: &str) {
        self.shell.toast(msg);
    }

    fn connect(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.list.connect_row_selected(move |_, row| {
            let (Some(this), Some(row)) = (weak.upgrade(), row) else {
                return;
            };
            let id = row.widget_name().to_string();
            if *this.selected.borrow() != id {
                *this.selected.borrow_mut() = id;
                this.show_card();
            }
        });
        // Only a click (or Enter) opens the card in the narrow layout:
        // show_list re-selects a row after every sync and write.
        let weak = Rc::downgrade(self);
        self.list.connect_row_activated(move |_, _| {
            if let Some(this) = weak.upgrade() {
                this.split.set_show_content(true);
            }
        });
        // The rows are built once per cache; a search only filters them.
        let weak = Rc::downgrade(self);
        self.list.set_filter_func(move |row| {
            weak.upgrade().is_none_or(|this| {
                this.visible
                    .borrow()
                    .as_ref()
                    .is_none_or(|ids| ids.contains(row.widget_name().as_str()))
            })
        });
        let weak = Rc::downgrade(self);
        self.filter.connect_selected_notify(move |_| {
            if let Some(this) = weak.upgrade() {
                this.follow_search();
            }
        });
        let weak = Rc::downgrade(self);
        self.search.connect_search_changed(move |_| {
            if let Some(this) = weak.upgrade() {
                this.follow_search();
            }
        });
        // Enter in the search box opens the first match.
        let weak = Rc::downgrade(self);
        self.search.connect_activate(move |_| {
            let Some(this) = weak.upgrade() else { return };
            if let Some(row) = this.first_visible() {
                this.list.select_row(Some(&row));
                this.split.set_show_content(true);
            }
        });

        let action = |name: &str, f: fn(&Rc<Window>)| {
            let a = gio::SimpleAction::new(name, None);
            let weak = Rc::downgrade(self);
            a.connect_activate(move |_, _| {
                if let Some(this) = weak.upgrade() {
                    f(&this);
                }
            });
            self.window.add_action(&a);
        };
        action("refresh", |this| {
            *this.last_error.borrow_mut() = None;
            this.sync();
        });
        action("new", |this| this.edit_draft(None, None, None));
        action("find", |this| {
            this.split.set_show_content(false);
            this.search.grab_focus();
        });
        action("edit", |this| {
            if let Some(c) = this.chosen() {
                this.edit_draft(Some(c), None, None);
            }
        });
        action("delete", |this| {
            if let Some(c) = this.chosen() {
                this.confirm_delete(c);
            }
        });
        action("mirror-review", |this| this.mirror_review());
        action("duplicates", |this| this.duplicates_dialog());
        action("mirror-not-now", |this| {
            let root = this.root.clone();
            this.ks_offer.set_revealed(false);
            background(
                move || {
                    root.map(|r| {
                        mirror::update(&r, |s| {
                            s.offer_declined = true;
                            Ok(())
                        })
                    })
                },
                |_| {},
            );
        });
        let weak = Rc::downgrade(self);
        self.ks_switch.connect_active_notify(move |switch| {
            let Some(this) = weak.upgrade() else { return };
            if switch.is_active() == this.ks_shown.get() {
                return;
            }
            if switch.is_active() {
                // on only through the review; the switch shows the outcome
                this.set_switch(false);
                this.mirror_review();
            } else {
                this.ks_shown.set(false);
                this.mirror_off();
            }
        });
        action("add-icloud", |this| this.link("icloud"));
        action("add-google", |this| this.link("google"));
        let a = gio::SimpleAction::new("relogin", Some(glib::VariantTy::STRING));
        let weak = Rc::downgrade(self);
        a.connect_activate(move |_, name| {
            if let (Some(this), Some(name)) = (weak.upgrade(), name.and_then(|n| n.get::<String>())) {
                this.relogin(name);
            }
        });
        self.window.add_action(&a);
        let a = gio::SimpleAction::new("unlink", Some(glib::VariantTy::STRING));
        let weak = Rc::downgrade(self);
        a.connect_activate(move |_, name| {
            if let (Some(this), Some(name)) = (weak.upgrade(), name.and_then(|n| n.get::<String>())) {
                this.confirm_unlink(name);
            }
        });
        self.window.add_action(&a);
    }

    /// The window's own keys: the shortcut overlay, and Ctrl+Q and Ctrl+W, which close the window as its close
    /// button does, so typed work is asked about first. The search has the keyboard most of the time, where `?` is a
    /// character, so the overlay is on F1 and Ctrl+?.
    fn install_keys(self: &Rc<Self>) {
        self.shortcuts.install(&self.window, true);
        omui::quit_keys(&self.window);
    }

    /// After a switch of language: what the window wrote from its state is written again. The widgets that hold one
    /// fixed string follow by themselves ([`i18n::bind_label`] and its kin); dialogs and toasts are made in the
    /// language of the moment.
    fn retranslate(self: &Rc<Self>) {
        let accounts = self.accounts.borrow().clone();
        self.show_accounts(&accounts);
        main_menu_items(&self.main_menu);
        self.no_card.set_title(&tr("No Contact Selected"));
        self.ks_offer
            .set_title(&tr("Your iCloud and Google contacts are separate. Mirror them?"));
        self.ks_row.set_title(&tr("Mirror contacts between iCloud and Google"));
        let line = self.mirror_line.borrow().clone();
        self.ks_row
            .set_subtitle(&line.map_or_else(|| tr("Off"), |line| mirror_text(&line)));
        self.show_duplicate_title();
        // The rows and the card, and the list's empty state with them.
        self.show_list();
    }

    fn chosen(&self) -> Option<Entry> {
        self.shown.borrow().get(&self.selected.borrow()).cloned()
    }

    /// The accounts and their caches, as the last fetch left them, without
    /// the network.
    fn load_cached(self: &Rc<Self>) {
        let (t, root) = (self.transport.clone(), self.root.clone());
        let weak = Rc::downgrade(self);
        background(
            move || {
                let (config, _) = config::first_run(config::icloud_signed_in)?;
                let book = open(&config, root, t)?;
                Ok::<_, Error>((names(&book), book.read(None, true).unwrap_or_default()))
            },
            move |result| {
                let Some(this) = weak.upgrade() else { return };
                if let Ok(Ok((accounts, shown))) = result
                    && this.shown.borrow().entries.is_empty()
                {
                    this.set_accounts(accounts);
                    *this.shown.borrow_mut() = shown;
                    this.show_list();
                }
            },
        );
    }

    /// Shows the accounts in the filter and the accounts menu, or the page
    /// that links the first one.
    fn set_accounts(self: &Rc<Self>, accounts: Vec<(String, String)>) {
        if *self.accounts.borrow() == accounts {
            return;
        }
        self.show_accounts(&accounts);
        *self.accounts.borrow_mut() = accounts;
    }

    /// Writes the account filter and the accounts menu for `accounts`, in the language of the moment.
    fn show_accounts(self: &Rc<Self>, accounts: &[(String, String)]) {
        let selected = self.filter_account();
        let mut names = vec![tr("All Accounts")];
        names.extend(accounts.iter().map(|(n, p)| account_title(n, p)));
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        self.filter_names.splice(0, self.filter_names.n_items(), &refs);
        let at = selected
            .and_then(|s| accounts.iter().position(|(n, _)| *n == s))
            .map_or(0, |i| i + 1);
        self.filter.set_selected(at as u32);
        self.filter.set_visible(accounts.len() > 1);
        // A new menu each time: GTK's popover names a submenu's page by its label, and a model changed in place adds
        // the pages again beside the old ones (a warning, and a menu that shows the first), as a switch of language
        // or a second linked account would.
        let menu = gio::Menu::new();
        let linked = gio::Menu::new();
        for (name, provider) in accounts {
            let one = gio::Menu::new();
            if provider == "google" {
                one.append(
                    Some(&tr("Sign In Again")),
                    Some(&gio::Action::print_detailed_name(
                        "win.relogin",
                        Some(&name.to_variant()),
                    )),
                );
            }
            one.append(
                Some(&tr("Unlink…")),
                Some(&gio::Action::print_detailed_name(
                    "win.unlink",
                    Some(&name.to_variant()),
                )),
            );
            linked.append_submenu(Some(&account_title(name, provider)), &one);
        }
        menu.append_section(None, &linked);
        let add = gio::Menu::new();
        if !accounts.iter().any(|(_, p)| p == "icloud") {
            add.append(Some(&tr("Add iCloud Account…")), Some("win.add-icloud"));
        }
        add.append(Some(&tr("Add Google Account…")), Some("win.add-google"));
        menu.append_section(None, &add);
        self.accounts_button.set_menu_model(Some(&menu));
        self.stack
            .set_visible_child_name(if accounts.is_empty() { "welcome" } else { "contacts" });
    }

    /// The account the filter shows; `None` for all.
    fn filter_account(&self) -> Option<String> {
        let i = self.filter.selected() as usize;
        (i > 0)
            .then(|| self.accounts.borrow().get(i - 1).map(|(n, _)| n.clone()))
            .flatten()
    }

    /// Links an account of `provider`: icloud-session's window, or Google in
    /// the browser. Blocks a worker thread until it is done.
    fn link(self: &Rc<Self>, provider: &'static str) {
        let root = self.root.clone();
        let weak = Rc::downgrade(self);
        self.toast(&match provider {
            "google" => tr("Finish signing in to Google in your browser"),
            _ => tr("Finish signing in to iCloud in its window"),
        });
        background(
            move || {
                let root = root.ok_or_else(|| Error::Other(tr("no data directory ($HOME is not set)")))?;
                // One more Google account each time: google, then google-2, …
                let name = accounts::next_name(provider)?;
                accounts::add(Some(&name), provider, &root)
            },
            move |result| {
                let Some(this) = weak.upgrade() else { return };
                match result {
                    Ok(Ok((name, _))) => {
                        this.toast(&tr!("Linked {}", account_title(&name, provider)));
                        let other = if provider == "google" { "icloud" } else { "google" };
                        if this.accounts.borrow().iter().any(|(_, p)| p == other) {
                            this.offer_after_link();
                        }
                        this.sync();
                    }
                    Ok(Err(e)) => this.toast(&e.to_string()),
                    Err(e) => this.toast(&e),
                }
            },
        );
    }

    fn relogin(self: &Rc<Self>, name: String) {
        let root = self.root.clone();
        let weak = Rc::downgrade(self);
        self.toast(&tr("Finish signing in in your browser"));
        background(
            move || {
                let root = root.ok_or_else(|| Error::Other(tr("no data directory ($HOME is not set)")))?;
                accounts::login(&name, &root)
            },
            move |result| {
                let Some(this) = weak.upgrade() else { return };
                match result {
                    Ok(Ok(note)) => {
                        this.toast(&note);
                        *this.last_error.borrow_mut() = None;
                        this.sync();
                    }
                    Ok(Err(e)) => this.toast(&e.to_string()),
                    Err(e) => this.toast(&e),
                }
            },
        );
    }

    fn confirm_unlink(self: &Rc<Self>, name: String) {
        let weak = Rc::downgrade(self);
        let shown = name.clone();
        confirm_destructive(
            &self.window,
            move |dialog| {
                dialog.set_heading(&tr!("Unlink {}?", shown));
                dialog.set_body(&tr(
                    "Its contacts stay in the account; this computer stops showing them and forgets its copy.",
                ));
                dialog.set_response_label("confirm", &tr("_Unlink"));
            },
            move || {
                let Some(this) = weak.upgrade() else { return };
                let root = this.root.clone();
                let weak = Rc::downgrade(&this);
                background(
                    move || {
                        let root = root.ok_or_else(|| Error::Other(tr("no data directory ($HOME is not set)")))?;
                        accounts::remove(&name, &root)
                    },
                    move |result| {
                        let Some(this) = weak.upgrade() else { return };
                        match result {
                            Ok(Ok(note)) => this.toast(&note),
                            Ok(Err(e)) => this.toast(&e.to_string()),
                            Err(e) => this.toast(&e),
                        }
                        this.selected.borrow_mut().clear();
                        this.shown.borrow_mut().entries.clear();
                        this.sync();
                    },
                );
            },
        );
    }

    /// Syncs on a worker thread, then shows the cache.
    fn sync(self: &Rc<Self>) {
        if self.syncing.replace(true) {
            return;
        }
        let (t, root) = (self.transport.clone(), self.root.clone());
        let weak = Rc::downgrade(self);
        background(
            move || {
                let (config, note) = config::first_run(config::icloud_signed_in)?;
                let book = open(&config, root, t)?;
                let accounts = names(&book);
                if accounts.is_empty() {
                    return Ok((accounts, Shown::default(), note));
                }
                Ok::<_, Error>((accounts, book.read(None, false)?, note))
            },
            move |result| {
                let Some(this) = weak.upgrade() else { return };
                this.syncing.set(false);
                match result {
                    Ok(Ok((accounts, shown, note))) => {
                        if let Some(n) = note {
                            this.toast(&n);
                        }
                        this.set_accounts(accounts);
                        this.show_warnings(&shown);
                        this.show_mirror();
                        this.show_duplicate_count(&shown);
                        // Most fetches change nothing: the rows stay.
                        let changed = *this.shown.borrow() != shown;
                        *this.shown.borrow_mut() = shown;
                        if changed {
                            this.show_list();
                        }
                    }
                    Ok(Err(e)) if e.is_icloud_sign_in() => this.banner.show(),
                    Ok(Err(e)) => this.error(&e.to_string()),
                    Err(e) => this.error(&e),
                }
            },
        );
    }

    /// iCloud signed out shows its banner; another account's trouble is a
    /// toast, once while it lasts.
    fn show_warnings(&self, shown: &Shown) {
        let icloud_out = shown.warnings.iter().any(|w| w.code == "sign_in_required");
        if icloud_out {
            self.banner.show();
        } else {
            self.banner.hide();
        }
        match shown.warnings.iter().find(|w| w.code != "sign_in_required") {
            Some(w) => self.error(&w.message),
            None => *self.last_error.borrow_mut() = None,
        }
    }

    /// Toasts an error once, not on every tick while it persists.
    fn error(&self, msg: &str) {
        if self.last_error.borrow().as_deref() != Some(msg) {
            self.toast(msg);
            *self.last_error.borrow_mut() = Some(msg.to_owned());
        }
    }

    /// Runs one write on a worker thread; shows the cache afterwards (the
    /// write updated it) and calls `done`, or shows the error and calls
    /// `failed`, which gives back what the user typed.
    fn write<T: Send + 'static>(
        self: &Rc<Self>,
        f: impl FnOnce(&Book) -> cloud_contacts::error::Result<T> + Send + 'static,
        done: impl FnOnce(&Rc<Self>, T) + 'static,
        failed: impl FnOnce(&Rc<Self>) + 'static,
    ) {
        let (t, root) = (self.transport.clone(), self.root.clone());
        let weak = Rc::downgrade(self);
        background(
            move || {
                let book = open(&config::load()?, root, t)?;
                let out = f(&book)?;
                // The write happened: caches that can't be read are shown
                // as a warning, not as a failed write.
                Ok::<_, Error>((out, book.read(None, true).map_err(|e| e.to_string())))
            },
            move |result| {
                let Some(this) = weak.upgrade() else { return };
                let problem = match result {
                    Ok(Ok((out, shown))) => {
                        match shown {
                            Ok(shown) => *this.shown.borrow_mut() = shown,
                            Err(e) => this.toast(&tr!("Saved; cannot read the local copy: {}", e)),
                        }
                        done(&this, out);
                        this.show_list();
                        this.mirror_run();
                        return;
                    }
                    Ok(Err(e)) if e.is_icloud_sign_in() => {
                        this.banner.show();
                        None
                    }
                    Ok(Err(e)) => Some(e.to_string()),
                    Err(e) => Some(e),
                };
                if let Some(problem) = problem {
                    this.toast(&problem);
                }
                failed(&this);
            },
        );
    }

    /// What `duplicates` finds in `shown`: groups within an account, and
    /// pairs across accounts (mirroring's).
    fn find_duplicates(&self, shown: &Shown) -> (Vec<dedupe::Group>, Vec<dedupe::Across>) {
        let apart = self
            .root
            .as_ref()
            .and_then(|r| dedupe::Apart::load(r).ok())
            .unwrap_or_default();
        let linked = self
            .root
            .as_ref()
            .and_then(|r| mirror::load(r).ok())
            .map(|s| {
                s.links
                    .into_iter()
                    .flat_map(|l| [(l.icloud.clone(), l.google.clone()), (l.google, l.icloud)])
                    .collect()
            })
            .unwrap_or_default();
        dedupe::find(&shown.entries, &apart, &linked)
    }

    /// The banner over the contacts while likely duplicates exist.
    fn show_duplicate_count(&self, shown: &Shown) {
        let (groups, _) = self.find_duplicates(shown);
        let likely: usize = groups
            .iter()
            .filter(|g| g.confidence == dedupe::Confidence::Likely)
            .map(|g| g.members.len())
            .sum();
        self.duplicates.set(likely);
        self.show_duplicate_title();
        self.dup_offer.set_revealed(likely > 0);
    }

    fn show_duplicate_title(&self) {
        let likely = self.duplicates.get();
        if likely > 0 {
            self.dup_offer
                .set_title(&trn!("{n} contacts look like duplicates of each other.", likely));
        }
    }

    /// Duplicates: those likely the same, merged all at once; those to
    /// check, merged or kept apart one group at a time; and pairs across
    /// accounts, which mirroring pairs.
    fn duplicates_dialog(self: &Rc<Self>) {
        let shown = self.shown.borrow().clone();
        let (groups, across) = self.find_duplicates(&shown);
        let (page, column) = omui::page(600);
        let dialog = omui::Dialog::new(Some("Duplicates"), None);
        dialog.set_heading_translated("Duplicates");
        dialog.add_response("close", "_Close");
        dialog.set_response_translated("close", "_Close");
        dialog.set_default_response("close");
        dialog.set_close_response("close");
        dialog.set_content_size(560, 640);
        let entries_of =
            |g: &dedupe::Group| -> Vec<Entry> { g.members.iter().map(|&m| shown.entries[m].clone()).collect() };
        let preview = |members: &[Entry]| -> (usize, String, String) {
            let cards: Vec<&cloud_contacts::model::Contact> = members.iter().map(|e| &e.contact).collect();
            let keep = dedupe::keeper(&cards);
            let merged = dedupe::merge_all(&cards, keep);
            let mut name = [
                &merged.prefix,
                &merged.first,
                &merged.middle,
                &merged.last,
                &merged.suffix,
            ]
            .iter()
            .filter(|p| !p.is_empty())
            .map(|p| p.as_str())
            .collect::<Vec<_>>()
            .join(" ");
            if name.is_empty() {
                name = members[keep].contact.display_name();
            }
            let mut parts = vec![trn!("{n} cards", members.len())];
            if !merged.phones.is_empty() {
                parts.push(trn!("{n} phones", merged.phones.len()));
            }
            if !merged.emails.is_empty() {
                parts.push(trn!("{n} emails", merged.emails.len()));
            }
            (keep, name, parts.join(" · "))
        };
        let likely: Vec<Vec<Entry>> = groups
            .iter()
            .filter(|g| g.confidence == dedupe::Confidence::Likely)
            .map(entries_of)
            .collect();
        if !likely.is_empty() {
            let g = omui::Group::new(
                &tr("Likely the Same"),
                &tr(
                    "Each becomes one card with every phone, email and address of them all. Every card is saved first to a backup file.",
                ),
            );
            let merge_all = gtk::Button::builder()
                .label(tr("Merge All"))
                .valign(gtk::Align::Center)
                .css_classes(["suggested-action"])
                .build();
            g.set_header_suffix(Some(merge_all.upcast_ref()));
            for members in &likely {
                let (_, name, detail) = preview(members);
                let names: Vec<String> = members.iter().map(|e| e.contact.display_name()).collect();
                g.add(&omui::Row::new(&name, &tr!("{} · from {}", detail, names.join(", "))).widget);
            }
            // Weak: the dialog owns the button.
            let (weak, d) = (Rc::downgrade(self), dialog.downgrade());
            merge_all.connect_clicked(move |_| {
                let (Some(this), Some(d)) = (weak.upgrade(), d.upgrade()) else {
                    return;
                };
                d.close();
                this.merge_groups(likely.clone());
            });
            column.append(&g.widget);
        }
        let check: Vec<(Vec<Entry>, String)> = groups
            .iter()
            .filter(|g| g.confidence == dedupe::Confidence::Check)
            .map(|g| (entries_of(g), g.why.join(", ")))
            .collect();
        if !check.is_empty() {
            let g = omui::Group::new(
                &tr("Check These"),
                &tr("They share a name or a number, but may be different people."),
            );
            for (members, why) in check {
                let names: Vec<String> = members.iter().map(|e| e.contact.display_name()).collect();
                let row = omui::Row::new(&names.join(" · "), &why);
                let apart = gtk::Button::builder()
                    .label(tr("Keep Apart"))
                    .valign(gtk::Align::Center)
                    .build();
                let merge = gtk::Button::builder()
                    .label(tr("Merge"))
                    .valign(gtk::Align::Center)
                    .build();
                row.add_suffix(&apart);
                row.add_suffix(&merge);
                let (weak, d, m) = (Rc::downgrade(self), dialog.downgrade(), members.clone());
                merge.connect_clicked(move |_| {
                    let (Some(this), Some(d)) = (weak.upgrade(), d.upgrade()) else {
                        return;
                    };
                    d.close();
                    this.merge_groups(vec![m.clone()]);
                });
                let (weak, r) = (Rc::downgrade(self), row.widget.downgrade());
                apart.connect_clicked(move |_| {
                    let Some(this) = weak.upgrade() else { return };
                    let ids: Vec<String> = members.iter().map(Entry::id).collect();
                    if let (Some(root), Some(r)) = (this.root.clone(), r.upgrade()) {
                        r.set_sensitive(false);
                        let weak = Rc::downgrade(&this);
                        background(
                            move || dedupe::Apart::add(&root, &ids).map_err(|e| e.to_string()),
                            move |result| {
                                let Some(this) = weak.upgrade() else { return };
                                match result {
                                    Ok(Ok(())) => {
                                        this.toast(&tr("Kept apart: not shown as duplicates again"));
                                        let shown = this.shown.borrow().clone();
                                        this.show_duplicate_count(&shown);
                                    }
                                    Ok(Err(e)) | Err(e) => this.toast(&e),
                                }
                            },
                        );
                    }
                });
                g.add(&row.widget);
            }
            column.append(&g.widget);
        }
        if !across.is_empty() {
            let g = omui::Group::new(
                &tr("In Both Accounts"),
                &tr("Mirroring iCloud and Google pairs these, keeping one in each account."),
            );
            for p in &across {
                let (a, b) = (&shown.entries[p.a], &shown.entries[p.b]);
                let title = tr!(
                    "{} ({}) · {} ({})",
                    a.contact.display_name(),
                    a.label(),
                    b.contact.display_name(),
                    b.label()
                );
                g.add(&omui::Row::new(&title, &p.why.join(", ")).widget);
            }
            column.append(&g.widget);
        }
        if groups.is_empty() && across.is_empty() {
            let none = omui::EmptyState::new(Some(name::USER), &tr("No Duplicates"), None);
            dialog.set_extra_child(Some(none.widget.upcast_ref()));
        } else {
            dialog.set_extra_child(Some(page.upcast_ref()));
        }
        dialog.present(&self.window);
    }

    /// Merges each group into its fullest card (see `dedupe::apply`), and
    /// names the backup file.
    fn merge_groups(self: &Rc<Self>, groups: Vec<Vec<Entry>>) {
        let Some(root) = self.root.clone() else { return };
        self.toast(&tr("Merging…"));
        self.write(
            move |book| {
                let mut backup = None;
                let mut n = 0;
                for g in &groups {
                    let refs: Vec<&Entry> = g.iter().collect();
                    let cards: Vec<&cloud_contacts::model::Contact> = g.iter().map(|e| &e.contact).collect();
                    let done = dedupe::apply(book, &root, &refs, dedupe::keeper(&cards), false)?;
                    backup = Some(done.backup);
                    n += 1;
                }
                Ok((n, backup))
            },
            |this, (n, backup)| {
                let home = std::env::var("HOME").unwrap_or_default();
                let file = backup
                    .map(|b| b.display().to_string())
                    .map(|f| match f.strip_prefix(&home) {
                        Some(rest) if !home.is_empty() => format!("~{rest}"),
                        _ => f,
                    })
                    .unwrap_or_default();
                this.toast(&trn!("Merged {n} groups; the cards are saved in {}", n, file));
                let shown = this.shown.borrow().clone();
                this.show_duplicate_count(&shown);
            },
            |_| {},
        );
    }

    /// Moves the switch without taking it for the person's move.
    fn set_switch(&self, on: bool) {
        self.ks_shown.set(on);
        self.ks_switch.set_active(on);
    }

    /// Shows mirroring's offer, switch and status line as the linked
    /// accounts and its state say (`mirror::shown`).
    fn show_mirror(self: &Rc<Self>) {
        let root = self.root.clone();
        let weak = Rc::downgrade(self);
        background(
            move || {
                let root = root?;
                let config = config::load().ok()?;
                let accounts: Vec<(String, bool)> = accounts::statuses(&config, &root)
                    .into_iter()
                    .map(|a| (a.provider, a.signed_in))
                    .collect();
                let st = mirror::load(&root).unwrap_or_default();
                Some(mirror::shown(&accounts, &st))
            },
            move |result| {
                let Some(this) = weak.upgrade() else { return };
                match result.ok().flatten() {
                    Some(mirror::Shown::Switch { on, line, offer }) => {
                        this.ks_list.set_visible(true);
                        this.set_switch(on);
                        this.ks_row.set_subtitle(&mirror_text(&line));
                        *this.mirror_line.borrow_mut() = Some(line);
                        this.ks_offer.set_revealed(offer);
                    }
                    _ => {
                        this.ks_list.set_visible(false);
                        this.ks_offer.set_revealed(false);
                    }
                }
            },
        );
    }

    /// Right after the second kind of account is linked.
    fn offer_after_link(self: &Rc<Self>) {
        // Made empty and then given its English, so an open dialog follows a switch of language.
        let dialog = omui::Dialog::new(Some(""), Some(""));
        dialog.set_heading_translated("Mirror Contacts Between iCloud and Google?");
        dialog.set_body_translated(
            "A contact added, changed or deleted in one would be in the other too. You can turn this on later below the contacts.",
        );
        dialog.add_response("later", "");
        dialog.add_response("on", "");
        dialog.set_response_translated("later", "_Not Now");
        dialog.set_response_translated("on", "_Turn On…");
        dialog.set_response_appearance("on", omui::Appearance::Suggested);
        dialog.set_default_response("on");
        dialog.set_close_response("later");
        let weak = Rc::downgrade(self);
        dialog.connect_response(move |_, id| {
            if let (Some(this), "on") = (weak.upgrade(), id) {
                this.mirror_review();
            }
        });
        dialog.present(&self.window);
    }

    /// The two accounts' providers, opened, for the engine.
    fn pair_book(root: Option<PathBuf>, t: Arc<SessionTransport>) -> cloud_contacts::error::Result<(PathBuf, Book)> {
        let root = root.ok_or_else(|| Error::Other(tr("no data directory ($HOME is not set)")))?;
        let book = open(&config::load()?, Some(root.clone()), t)?;
        Ok((root, book))
    }

    /// Turn On…: what turning on would do, with the doubtful pairs for the
    /// person to call, then on.
    fn mirror_review(self: &Rc<Self>) {
        let (t, root) = (self.transport.clone(), self.root.clone());
        let weak = Rc::downgrade(self);
        self.toast(&tr("Looking at both accounts…"));
        background(
            move || -> Result<(mirror::Plan, Vec<(String, String)>), String> {
                let (root, book) = Self::pair_book(root, t).map_err(|e| e.to_string())?;
                let mirror::Pairing::Ready(i, g) = mirror::pairing_of(&book) else {
                    return Err(tr("mirroring contacts needs an iCloud and a Google account"));
                };
                let engine = mirror::Engine::new(&root, &*i.provider_impl, &*g.provider_impl);
                let plan = engine.plan().map_err(|e| e.to_string())?;
                // names for the doubtful pairs
                let shown = book.read(None, true).map_err(|e| e.to_string())?;
                let name = |acc: &str, id: &str| {
                    shown
                        .get(&format!("{acc}:{id}"))
                        .map(|e| e.contact.display_name())
                        .unwrap_or_else(|| id.to_string())
                };
                let names = plan
                    .doubtful
                    .iter()
                    .map(|p| (name(&i.name, &p.icloud), name(&g.name, &p.google)))
                    .collect();
                Ok((plan, names))
            },
            move |result| {
                let Some(this) = weak.upgrade() else { return };
                match result {
                    Ok(Ok((plan, names))) => this.review_dialog(plan, names),
                    Ok(Err(e)) | Err(e) => this.toast(&e),
                }
            },
        );
    }

    fn review_dialog(self: &Rc<Self>, plan: mirror::Plan, names: Vec<(String, String)>) {
        let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
        // Whole sentences, one count each, so that each language gives each count its own plural form.
        let summary = [
            trn!("{n} contacts in both accounts will be joined.", plan.pairs.len()),
            trn!("{n} contacts will be copied to Google.", plan.to_google),
            trn!("{n} contacts will be copied to iCloud.", plan.to_icloud),
            tr(
                "From then on, a contact added, changed or deleted in one is in the other too, while this computer is on. Photos and Google's groups stay where they are.",
            ),
        ]
        .join(" ");
        body.append(&gtk::Label::builder().label(&summary).wrap(true).xalign(0.0).build());
        let mut choices = Vec::new();
        if !plan.doubtful.is_empty() {
            body.append(
                &gtk::Label::builder()
                    .label(tr("These may be one person. Neither is copied until you decide:"))
                    .wrap(true)
                    .xalign(0.0)
                    .css_classes(["heading"])
                    .build(),
            );
            let list = gtk::ListBox::builder()
                .selection_mode(gtk::SelectionMode::None)
                .css_classes(["boxed-list"])
                .build();
            let options = gtk::StringList::new(&[&tr("Decide Later"), &tr("Same Person"), &tr("Different People")]);
            for (p, (i, g)) in plan.doubtful.iter().zip(&names) {
                let (row, choice) = omui::combo_row(&tr!("{} (iCloud) · {} (Google)", i, g), &p.why, &options, 0);
                list.append(&row.widget);
                choices.push((p.clone(), choice));
            }
            body.append(
                &gtk::ScrolledWindow::builder()
                    .child(&list)
                    .propagate_natural_height(true)
                    .max_content_height(320)
                    .hscrollbar_policy(gtk::PolicyType::Never)
                    .build(),
            );
        }
        let dialog = omui::Dialog::new(Some(""), None);
        dialog.set_heading_translated("Mirror Contacts Between iCloud and Google");
        dialog.set_extra_child(Some(body.upcast_ref()));
        dialog.add_response("cancel", "");
        dialog.add_response("on", "");
        dialog.set_response_translated("cancel", "_Cancel");
        dialog.set_response_translated("on", "_Turn On");
        dialog.set_response_appearance("on", omui::Appearance::Suggested);
        dialog.set_default_response("on");
        dialog.set_close_response("cancel");
        let weak = Rc::downgrade(self);
        dialog.connect_response(move |_, id| {
            let (Some(this), "on") = (weak.upgrade(), id) else {
                return;
            };
            let decisions: Vec<(String, String, mirror::Decision)> = choices
                .iter()
                .filter_map(|(p, row)| {
                    let d = match row.selected() {
                        1 => mirror::Decision::Same,
                        2 => mirror::Decision::Different,
                        _ => return None,
                    };
                    Some((p.icloud.clone(), p.google.clone(), d))
                })
                .collect();
            let (t, root, plan) = (this.transport.clone(), this.root.clone(), plan.clone());
            let weak = Rc::downgrade(&this);
            background(
                move || -> Result<String, String> {
                    let (root, book) = Self::pair_book(root, t).map_err(|e| e.to_string())?;
                    mirror::turn_on(&root, &plan, &decisions).map_err(|e| e.to_string())?;
                    let warning = mirror::timer(true);
                    let mirror::Pairing::Ready(i, g) = mirror::pairing_of(&book) else {
                        return Err(tr("an account went away"));
                    };
                    let mut engine = mirror::Engine::new(&root, &*i.provider_impl, &*g.provider_impl);
                    let done = match engine.run(false) {
                        Ok(_) => tr("iCloud and Google contacts are mirrored now"),
                        Err(e) => e.to_string(),
                    };
                    Ok(warning.map_or(done.clone(), |w| tr!("{} ({})", done, w)))
                },
                move |result| {
                    let Some(this) = weak.upgrade() else { return };
                    match result {
                        Ok(Ok(note)) => this.toast(&note),
                        Ok(Err(e)) | Err(e) => this.toast(&e),
                    }
                    this.sync();
                },
            );
        });
        dialog.present(&self.window);
    }

    fn mirror_off(self: &Rc<Self>) {
        let root = self.root.clone();
        let weak = Rc::downgrade(self);
        background(
            move || -> Option<String> {
                let root = root?;
                mirror::turn_off(&root).ok()?;
                Some(
                    mirror::timer(false)
                        .unwrap_or_else(|| tr("Turned off: both accounts keep their contacts as they are")),
                )
            },
            move |result| {
                let Some(this) = weak.upgrade() else { return };
                if let Ok(Some(note)) = result {
                    this.toast(&note);
                }
                this.show_mirror();
            },
        );
    }

    /// A run now (after a change made here), when it is on.
    fn mirror_run(self: &Rc<Self>) {
        let (t, root) = (self.transport.clone(), self.root.clone());
        let weak = Rc::downgrade(self);
        background(
            move || {
                let (root, book) = Self::pair_book(root, t).ok()?;
                if !mirror::load(&root).ok()?.on {
                    return None;
                }
                let mirror::Pairing::Ready(i, g) = mirror::pairing_of(&book) else {
                    return None;
                };
                let mut engine = mirror::Engine::new(&root, &*i.provider_impl, &*g.provider_impl);
                Some(engine.run(false).map(drop).map_err(|e| e.to_string()))
            },
            move |result| {
                let Some(this) = weak.upgrade() else { return };
                if let Ok(Some(Err(e))) = result {
                    this.error(&e);
                }
                this.show_mirror();
            },
        );
    }

    fn confirm_delete(self: &Rc<Self>, c: Entry) {
        let name = c.contact.display_name();
        let (shown, google) = (name.clone(), c.provider == "google");
        let weak = Rc::downgrade(self);
        confirm_destructive(
            &self.window,
            move |dialog| {
                dialog.set_heading(&tr!("Delete {}?", shown));
                dialog.set_body(&if google {
                    tr("The contact is deleted from your Google account on all your devices. Google keeps it in its Trash for 30 days.")
                } else {
                    tr("The contact is deleted from iCloud on all your devices. iCloud keeps no Recently Deleted for contacts.")
                });
                dialog.set_response_label("confirm", &tr("_Delete"));
            },
            move || {
                let Some(this) = weak.upgrade() else { return };
                this.write(
                    move |book| book.delete(&c),
                    move |this, warning| {
                        this.selected.borrow_mut().clear();
                        this.split.set_show_content(false);
                        this.toast(&warning.unwrap_or_else(|| tr!("Deleted {}", name)));
                    },
                    |_| {},
                );
            },
        );
    }

    /// Asks before typed work is lost. Only Discard loses it: Keep Editing, Escape and Enter all run `keep`.
    fn ask_discard(self: &Rc<Self>, keep: impl FnOnce(&Rc<Self>) + 'static, discard: impl FnOnce(&Rc<Self>) + 'static) {
        self.asking.set(true);
        let dialog = omui::Dialog::new(Some(""), Some(""));
        dialog.set_heading_translated("Discard your changes?");
        dialog.set_body_translated("What you typed for this contact is not saved.");
        dialog.add_response("keep", "");
        dialog.add_response("discard", "");
        dialog.set_response_translated("keep", "_Keep Editing");
        dialog.set_response_translated("discard", "_Discard");
        dialog.set_response_appearance("discard", omui::Appearance::Destructive);
        dialog.set_default_response("keep");
        dialog.set_close_response("keep");
        let weak = Rc::downgrade(self);
        let answers = Cell::new(Some((keep, discard)));
        dialog.connect_response(move |_, id| {
            let (Some(this), Some((keep, discard))) = (weak.upgrade(), answers.take()) else {
                return;
            };
            this.asking.set(false);
            if id == "discard" {
                discard(&this);
            } else {
                keep(&this);
            }
        });
        dialog.present(&self.window);
    }

    /// The add (`contact` `None`) or edit dialog, filled from the contact's
    /// values, or from `draft`: what was typed before a save that failed, or
    /// before a Cancel that was taken back. `account` is the account a new
    /// contact was going to then.
    fn edit_draft(self: &Rc<Self>, contact: Option<Entry>, draft: Option<Draft>, account: Option<String>) {
        let accounts = self.accounts.borrow().clone();
        if contact.is_none() && accounts.is_empty() {
            self.toast(&tr("Link an account first"));
            return;
        }
        let before = contact.as_ref().map(|e| Draft::of(&e.contact)).unwrap_or_default();
        let draft = draft.unwrap_or_else(|| before.clone());
        let dialog = omui::Dialog::new(Some(""), None);
        if contact.is_some() {
            dialog.set_heading_translated("Edit Contact");
        } else {
            dialog.set_heading_translated("New Contact");
        }
        let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
        // A new contact goes to one account: the one the sidebar shows, else
        // the first; with several, the person picks.
        let titles: Vec<String> = accounts.iter().map(|(n, p)| account_title(n, p)).collect();
        let account_list = gtk::StringList::new(&titles.iter().map(String::as_str).collect::<Vec<_>>());
        let start = account
            .or_else(|| self.filter_account())
            .and_then(|f| accounts.iter().position(|(n, _)| *n == f))
            .unwrap_or(0);
        let (account_row, account_choice) = omui::combo_row(&tr("Account"), "", &account_list, start as u32);
        let group = |rows: &[&omui::Row]| {
            let list = gtk::ListBox::builder()
                .selection_mode(gtk::SelectionMode::None)
                .css_classes(["boxed-list"])
                .build();
            for r in rows {
                list.append(&r.widget);
            }
            body.append(&list);
        };
        if contact.is_none() && accounts.len() > 1 {
            group(&[&account_row]);
        }
        let scalars: Vec<(Field, (omui::Row, gtk::Entry))> = SCALARS
            .iter()
            .map(|(f, _)| (*f, omui::entry_row(&field_title(*f), draft.scalar(*f))))
            .collect();
        group(&scalars[..3].iter().map(|(_, (r, _))| r).collect::<Vec<_>>());
        group(&scalars[3..].iter().map(|(_, (r, _))| r).collect::<Vec<_>>());
        let scalars: Vec<(Field, gtk::Entry)> = scalars.into_iter().map(|(f, (_, e))| (f, e)).collect();
        // Return in a one-line field saves, as it does in a form
        for (_, e) in &scalars {
            e.set_activates_default(true);
        }
        // One row per value, titled by its label, and an empty one to add.
        let mut multis: Vec<(Multi, Vec<ValueRow>)> = Vec::new();
        for (m, add_title) in [
            (Multi::Phone, tr("Add phone (work=+1 555 0100)")),
            (Multi::Email, tr("Add email")),
            (Multi::Url, tr("Add URL")),
        ] {
            let mut rows: Vec<(Option<Row>, (omui::Row, gtk::Entry))> = draft
                .values(m)
                .iter()
                .map(|r| (Some(r.clone()), omui::entry_row(&label_title(&r.label), &r.value)))
                .collect();
            rows.push((None, omui::entry_row(&add_title, "")));
            group(&rows.iter().map(|(_, (r, _))| r).collect::<Vec<_>>());
            for (_, (_, e)) in &rows {
                e.set_activates_default(true);
            }
            multis.push((m, rows.into_iter().map(|(shown, (_, e))| (shown, e)).collect()));
        }
        let notes = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::WordChar)
            .top_margin(8)
            .bottom_margin(8)
            .left_margin(8)
            .right_margin(8)
            .build();
        notes.buffer().set_text(&draft.notes);
        body.append(
            &gtk::Frame::builder()
                .child(
                    &gtk::ScrolledWindow::builder()
                        .child(&notes)
                        .min_content_height(70)
                        .build(),
                )
                .build(),
        );
        let problem = gtk::Label::builder()
            .css_classes(["error", "caption"])
            .xalign(0.0)
            .wrap(true)
            .visible(false)
            .build();
        // The fields scroll; what is wrong with them stays under them, over the Save it holds back. At the end of
        // the fields it was out of sight below them, and Save was off for no reason one could see.
        let fields = gtk::ScrolledWindow::builder()
            .child(&body)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(520)
            .accessible_role(gtk::AccessibleRole::Form)
            .build();
        fields.update_property(&[gtk::accessible::Property::Label(&tr("Fields"))]);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        content.append(&fields);
        content.append(&problem);
        dialog.set_extra_child(Some(content.upcast_ref()));
        dialog.add_response("cancel", "");
        dialog.add_response("save", "");
        dialog.set_response_translated("cancel", "_Cancel");
        dialog.set_response_translated("save", "_Save");
        dialog.set_response_appearance("save", omui::Appearance::Suggested);
        dialog.set_default_response("save");
        dialog.set_close_response("cancel");

        let scalars = Rc::new(scalars);
        let multis = Rc::new(multis);
        let read = Rc::new({
            let (scalars, multis, notes) = (scalars.clone(), multis.clone(), notes.clone());
            move || {
                let buffer = notes.buffer();
                Draft {
                    scalars: scalars.iter().map(|(f, r)| (*f, r.text().to_string())).collect(),
                    multis: multis
                        .iter()
                        .map(|(m, rows)| {
                            let values = rows
                                .iter()
                                .map(|(shown, r)| {
                                    let (label, value) = typed(*m, shown.as_ref().map(|s| s.label.as_str()), &r.text());
                                    Row {
                                        origin: shown.as_ref().and_then(|s| s.origin),
                                        label,
                                        value,
                                    }
                                })
                                .filter(|r| !r.value.is_empty())
                                .collect();
                            (*m, values)
                        })
                        .collect(),
                    notes: buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).to_string(),
                    middle_name: false,
                }
            }
        });
        // Save is enabled only while the draft is valid, so a mistake never
        // closes the dialog and loses the edits. Weak: the dialog owns the
        // entries this is connected to.
        let validate = Rc::new({
            let (dialog, problem, read, before) = (dialog.downgrade(), problem.clone(), read.clone(), before.clone());
            move || {
                let Some(dialog) = dialog.upgrade() else { return };
                let text = read().problem(&before).map(|p| problem_text(&p));
                problem.set_visible(text.is_some());
                problem.set_label(text.as_deref().unwrap_or(""));
                dialog.set_response_enabled("save", text.is_none());
            }
        });
        for (_, r) in scalars.iter() {
            let validate = validate.clone();
            r.connect_changed(move |_| validate());
        }
        for (_, rows) in multis.iter() {
            for (_, r) in rows {
                let validate = validate.clone();
                r.connect_changed(move |_| validate());
            }
        }
        // Now, and after a switch of language, so what holds Save back is said in the new one.
        dialog.bind({
            let validate = validate.clone();
            move |_| validate()
        });

        // Unsaved is what saving would change, not whether a key was pressed: an edit typed and undone, or typed
        // back to what the contact has, is no work to ask about.
        *self.editing.borrow_mut() = Some(Rc::new({
            let (read, before) = (read.clone(), before.clone());
            move || !read().changes(&before).is_empty()
        }));
        let weak = Rc::downgrade(self);
        dialog.connect_response(move |_, id| {
            let Some(this) = weak.upgrade() else { return };
            let typed = read();
            let changes = typed.changes(&before);
            let account = accounts
                .get(account_choice.selected() as usize)
                .map(|(n, _)| n.clone())
                .unwrap_or_default();
            let retry = {
                let (contact, typed, account) = (contact.clone(), typed.clone(), account.clone());
                move |this: &Rc<Window>| this.edit_draft(contact, Some(typed), Some(account))
            };
            if id != "save" {
                // Cancel and Escape close the dialog. With work in it they ask first, and anything but Discard
                // opens it again as it was; until then the window still counts the work as unsaved.
                if changes.is_empty() {
                    this.editing.take();
                } else {
                    this.ask_discard(retry, |this| {
                        this.editing.take();
                    });
                }
                return;
            }
            this.editing.take();
            match contact.clone() {
                None => this.write(
                    {
                        let account = account.clone();
                        move |book| book.add(&account, &changes, false)
                    },
                    move |this, saved| {
                        *this.selected.borrow_mut() = format!("{account}:{}", saved.value.id());
                        this.toast(
                            &saved
                                .warning
                                .unwrap_or_else(|| tr!("Added {}", saved.value.display_name())),
                        );
                    },
                    retry,
                ),
                Some(_) if changes.is_empty() => {}
                Some(c) => this.write(
                    move |book| book.update(&c, &changes),
                    |this, saved| {
                        if let Some(w) = saved.warning {
                            this.toast(&w);
                        }
                    },
                    retry,
                ),
            }
        });
        dialog.present(&self.window);
    }

    /// Rebuilds the sidebar from the cache (keeping the selection), then
    /// the card.
    fn show_list(self: &Rc<Self>) {
        let shown = self.shown.borrow();
        let selected = self.selected.borrow().clone();
        let several = self.accounts.borrow().len() > 1;
        // The rows one by one: `remove_all` takes the list's placeholder with them.
        while let Some(row) = self.list.row_at_index(0) {
            self.list.remove(&row);
        }
        for e in &shown.entries {
            let row = contact_row(e, several);
            self.list.append(&row);
            if e.id() == selected {
                self.list.select_row(Some(&row));
            }
        }
        drop(shown);
        self.follow_search();
        self.show_card();
    }

    /// Filters the rows by the search text. In the wide layout a contact
    /// is always shown, as Contacts does: the first match when the chosen
    /// one is filtered out.
    fn follow_search(self: &Rc<Self>) {
        let query = self.search.text();
        let account = self.filter_account();
        *self.visible.borrow_mut() = (!query.trim().is_empty() || account.is_some()).then(|| {
            let shown = self.shown.borrow();
            shown
                .entries
                .iter()
                .filter(|e| account.as_ref().is_none_or(|a| *a == e.account))
                .filter(|e| query.trim().is_empty() || e.contact.matches(&query))
                .map(Entry::id)
                .collect()
        });
        self.list.invalidate_filter();
        // A search that finds nobody is not an account without contacts.
        self.list_empty.set_title(&if query.trim().is_empty() {
            tr("No Contacts")
        } else {
            tr("No Results")
        });
        let shown = self.list.selected_row().is_some_and(|r| r.is_child_visible());
        if !shown && !self.split.is_collapsed() {
            let first = self.first_visible();
            *self.selected.borrow_mut() = first.as_ref().map(|r| r.widget_name().to_string()).unwrap_or_default();
            self.list.select_row(first.as_ref());
            self.show_card();
        }
    }

    fn first_visible(&self) -> Option<gtk::ListBoxRow> {
        (0..)
            .map_while(|i| self.list.row_at_index(i))
            .find(|r| r.is_child_visible())
    }

    fn show_card(self: &Rc<Self>) {
        let chosen = self.chosen();
        self.edit_button.set_sensitive(chosen.is_some());
        self.delete_button.set_sensitive(chosen.is_some());
        let Some(entry) = chosen else {
            self.card_stack.set_visible_child_name("empty");
            return;
        };
        let c = &entry.contact;
        self.card_stack.set_visible_child_name("card");
        omui::clear(&self.card);
        let name = c.display_name();
        let avatar = omui::avatar(96, &name);
        avatar.set_margin_top(12);
        self.card.append(&avatar);
        self.card.append(
            &gtk::Label::builder()
                .label(&name)
                .css_classes(["title-1"])
                .wrap(true)
                .justify(gtk::Justification::Center)
                .selectable(true)
                .build(),
        );
        let job = c.work(" · ");
        let nickname = c.field(Field::Nickname);
        let account = tr!("In {}", account_title(&entry.account, &entry.provider));
        for line in [job.as_str(), nickname, account.as_str()] {
            if !line.is_empty() {
                self.card.append(
                    &gtk::Label::builder()
                        .label(line)
                        .css_classes(["dim-label"])
                        .wrap(true)
                        .selectable(true)
                        .build(),
                );
            }
        }
        let group = |rows: Vec<omui::Row>| {
            if rows.is_empty() {
                return;
            }
            let list = gtk::ListBox::builder()
                .selection_mode(gtk::SelectionMode::None)
                .css_classes(["boxed-list"])
                .margin_top(6)
                .build();
            for r in rows {
                list.append(&r.widget);
            }
            self.card.append(&list);
        };
        let action = |m: Multi, v: &str| match m {
            Multi::Phone => Action::Copy,
            Multi::Email => Action::Open(format!("mailto:{v}")),
            Multi::Url if v.contains("://") => Action::Open(v.to_owned()),
            Multi::Url => Action::Open(format!("https://{v}")),
        };
        for m in [Multi::Phone, Multi::Email, Multi::Url] {
            let rows = c
                .values(m)
                .into_iter()
                .map(|l| self.value_row(&label_title(&l.label), &l.value, action(m, &l.value)))
                .collect();
            group(rows);
        }
        let addresses = c
            .addresses()
            .into_iter()
            .map(|a| {
                let city = [a.postal_code.as_str(), a.city.as_str(), a.state.as_str()]
                    .into_iter()
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join(" ");
                let text = [a.street.clone(), city, a.country.clone()]
                    .into_iter()
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n");
                self.value_row(&label_title(&a.label), &text, Action::Copy)
            })
            .collect();
        group(addresses);
        let birthday = c.field(Field::Birthday);
        if !birthday.is_empty() {
            // Shown as the language writes a date; a copy is the date as it is stored, which any app reads.
            group(vec![self.value_row_showing(
                &tr("birthday"),
                &birthday_text(birthday),
                &model::birthday_display(birthday),
                Action::Copy,
            )]);
        }
        let notes = c.field(Field::Notes);
        if !notes.is_empty() {
            group(vec![self.value_row(&tr("notes"), notes, Action::Copy)]);
        }
    }

    /// A row showing `value` under `label`; a click copies or opens it.
    fn value_row(self: &Rc<Self>, label: &str, value: &str, action: Action) -> omui::Row {
        self.value_row_showing(label, value, value, action)
    }

    /// A row showing `shown` under `label`, whose click copies `value` (or opens it).
    fn value_row_showing(self: &Rc<Self>, label: &str, shown: &str, value: &str, action: Action) -> omui::Row {
        let row = omui::Row::new(label, shown).property();
        row.set_subtitle_selectable(true);
        let (icon, tip) = match &action {
            Action::Copy => (name::COPY, tr("Copy")),
            Action::Open(_) => (name::EXTERNAL_LINK, tr("Open")),
        };
        let hint = omui::icon(icon);
        hint.set_tooltip_text(Some(&tip));
        row.add_suffix(&hint);
        let weak = Rc::downgrade(self);
        let value = value.to_owned();
        row.connect_activated(move || {
            let Some(this) = weak.upgrade() else { return };
            match &action {
                Action::Copy => {
                    this.window.clipboard().set_text(&value);
                    this.toast(&tr("Copied"));
                }
                Action::Open(uri) => {
                    let weak = Rc::downgrade(&this);
                    gtk::UriLauncher::new(uri).launch(Some(&this.window), gio::Cancellable::NONE, move |r| {
                        if let (Err(e), Some(this)) = (r, weak.upgrade()) {
                            this.toast(&tr!("Cannot open it: {}", e));
                        }
                    });
                }
            }
        });
        row
    }
}

enum Action {
    Copy,
    Open(String),
}

/// A row of the dialog for one phone, email or URL: the draft's row it
/// shows (its label and the stored value it came from), or `None` for the
/// row that adds one, and the field typed in.
type ValueRow = (Option<Row>, gtk::Entry);

/// How an account is named in the window: "iCloud", "Google", or the
/// service and its name when that differs ("Google (work)").
fn account_title(name: &str, provider: &str) -> String {
    let label = cloud_contacts::book::label(provider);
    if name == provider {
        label.to_string()
    } else {
        tr!("{} ({})", label, name)
    }
}

/// `several`: say which account the contact is in.
fn contact_row(e: &Entry, several: bool) -> gtk::ListBoxRow {
    let (c, me) = (&e.contact, e.me);
    let name = c.display_name();
    let b = gtk::Box::builder().spacing(10).margin_top(4).margin_bottom(4).build();
    b.append(&omui::avatar(32, &name));
    let text = gtk::Box::new(gtk::Orientation::Vertical, 0);
    text.set_valign(gtk::Align::Center);
    text.append(
        &gtk::Label::builder()
            .label(&name)
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build(),
    );
    let company = c.field(Field::Company);
    let sub = if me {
        tr("My Card")
    } else {
        let company = if c.is_company() { "" } else { company };
        let account = if several {
            cloud_contacts::book::label(&e.provider)
        } else {
            ""
        };
        [company, account]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" · ")
    };
    if !sub.is_empty() {
        text.append(
            &gtk::Label::builder()
                .label(&sub)
                .xalign(0.0)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .css_classes(["dim-label", "caption"])
                .build(),
        );
    }
    b.append(&text);
    let row = gtk::ListBoxRow::builder().child(&b).build();
    // A screen reader would otherwise read the avatar's initials first.
    let spoken = if sub.is_empty() { name } else { tr!("{}, {}", name, sub) };
    row.update_property(&[gtk::accessible::Property::Label(&spoken)]);
    row.set_widget_name(&e.id());
    row
}

fn build(app: &gtk::Application, banner: SignInBanner) -> Window {
    // Sidebar: search and the contacts.
    let search = gtk::SearchEntry::builder()
        .margin_start(8)
        .margin_end(8)
        .margin_bottom(6)
        .build();
    i18n::bind(&search, |search| {
        search.set_placeholder_text(Some(&tr("Search (Ctrl+F)")));
    });
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::Single)
        .activate_on_single_click(true)
        .build();
    let list_empty = omui::EmptyState::new(Some(name::USER), &tr("No Contacts"), None).compact();
    list.set_placeholder(Some(&list_empty.widget));
    let title = omui::Title::new(&tr("Contacts"), "");
    let sidebar_header = omui::header(Some(title.widget.upcast_ref()));
    i18n::on_change(&sidebar_header, move || title.set_title(&tr("Contacts")));
    // Refresh (Ctrl+R) and Duplicates in one menu: with three buttons the
    // sidebar still has room to say "Contacts". Its items are written by
    // `main_menu_items`, again in a new language.
    let main_menu = gio::Menu::new();
    main_menu_items(&main_menu);
    let main_button = omui::icon_menu_button(name::MENU, "", &main_menu);
    i18n::bind_tooltip(&main_button, "Main Menu");
    sidebar_header.pack_start(&main_button);
    let accounts_button = omui::icon_menu_button(name::USERS, "", &gio::Menu::new());
    i18n::bind_tooltip(&accounts_button, "Accounts");
    sidebar_header.pack_end(&accounts_button);
    let new = omui::icon_button(name::ADD, "");
    i18n::bind_tooltip(&new, "New Contact (Ctrl+N)");
    new.set_action_name(Some("win.new"));
    sidebar_header.pack_end(&new);
    let filter_names = gtk::StringList::new(&[&tr("All Accounts")]);
    let filter = gtk::DropDown::builder()
        .model(&filter_names)
        .margin_start(8)
        .margin_end(8)
        .margin_bottom(6)
        .visible(false)
        .build();
    i18n::bind_tooltip(&filter, "Show one account's contacts");
    let sidebar_body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .css_classes(["sidebar"])
        .vexpand(true)
        .build();
    sidebar_body.append(&filter);
    sidebar_body.append(&search);
    sidebar_body.append(&gtk::ScrolledWindow::builder().child(&list).vexpand(true).build());
    let (ks_row, ks_switch) = omui::switch_row(&tr("Mirror contacts between iCloud and Google"), &tr("Off"), false);
    let ks_list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .margin_start(8)
        .margin_end(8)
        .margin_top(6)
        .margin_bottom(8)
        .visible(false)
        .build();
    ks_list.append(&ks_row.widget);
    // In a scrolled window, whose least height is the same at any width: in
    // a narrow tile too short for the sidebar, GTK would otherwise widen the
    // window until this title takes one line less.
    sidebar_body.append(
        &gtk::ScrolledWindow::builder()
            .child(&ks_list)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .build(),
    );
    let sidebar = gtk::Box::new(gtk::Orientation::Vertical, 0);
    sidebar.append(&sidebar_header);
    sidebar.append(&sidebar_body);

    // Content: the chosen card, with a way back to the list when the two
    // do not fit side by side.
    let back = omui::icon_button(name::BACK, "");
    i18n::bind_tooltip(&back, "Contacts");
    let edit_button = omui::icon_button(name::EDIT, "");
    i18n::bind_tooltip(&edit_button, "Edit (Ctrl+E)");
    edit_button.set_action_name(Some("win.edit"));
    let delete_button = omui::icon_button(name::DELETE, "");
    i18n::bind_tooltip(&delete_button, "Delete");
    delete_button.set_action_name(Some("win.delete"));
    let content_header = omui::header_without_title();
    content_header.pack_start(&back);
    content_header.pack_end(&delete_button);
    content_header.pack_end(&edit_button);
    let card = gtk::Box::new(gtk::Orientation::Vertical, 6);
    let clamp = omui::clamp(&card, 560);
    clamp.set_margin_top(12);
    clamp.set_margin_bottom(24);
    clamp.set_margin_start(12);
    clamp.set_margin_end(12);
    let card_stack = gtk::Stack::new();
    card_stack.add_named(
        &gtk::ScrolledWindow::builder()
            .child(&clamp)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .build(),
        Some("card"),
    );
    let no_card = omui::EmptyState::new(Some(name::USER), &tr("No Contact Selected"), None);
    card_stack.add_named(&no_card.widget, Some("empty"));
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&content_header);
    content.append(&card_stack);

    let split = omui::Split::new(&sidebar, &content, (240, 320), Some(600));
    back.set_visible(false);
    split.connect_changed({
        let back = back.downgrade();
        move |split| {
            if let Some(back) = back.upgrade() {
                back.set_visible(split.is_collapsed());
            }
        }
    });
    back.connect_clicked({
        let split = split.downgrade();
        move |_| {
            if let Some(split) = split.upgrade() {
                split.set_show_content(false);
            }
        }
    });
    let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    body.append(&banner.widget);
    let ks_offer = omui::Banner::new(&tr("Your iCloud and Google contacts are separate. Mirror them?"), None);
    let not_now = ks_offer.add_button("", false);
    i18n::bind_label(&not_now, "Not Now");
    not_now.set_action_name(Some("win.mirror-not-now"));
    let turn_on = ks_offer.add_button("", true);
    i18n::bind_label(&turn_on, "Turn On…");
    turn_on.set_action_name(Some("win.mirror-review"));
    body.append(&ks_offer.widget);
    let dup_offer = omui::Banner::new("", None);
    let review = dup_offer.add_button("", false);
    i18n::bind_label(&review, "Review…");
    review.set_action_name(Some("win.duplicates"));
    body.append(&dup_offer.widget);
    split.widget.set_vexpand(true);
    body.append(&split.widget);
    // With no account linked: a page that links one.
    let welcome_buttons = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .halign(gtk::Align::Center)
        .build();
    for action in ["win.add-icloud", "win.add-google"] {
        let button = gtk::Button::builder()
            .action_name(action)
            .css_classes(["pill", "suggested-action"])
            .build();
        if action == "win.add-icloud" {
            i18n::bind_label(&button, "Add iCloud Account…");
        } else {
            i18n::bind_label(&button, "Add Google Account…");
        }
        welcome_buttons.append(&button);
    }
    let welcome_page = omui::EmptyState::new(
        Some(name::USER),
        &tr("Your Contacts"),
        Some(&tr(
            "Link your iCloud or Google account to see its contacts here. Each account keeps its own.",
        )),
    );
    welcome_page.set_child(Some(welcome_buttons.upcast_ref()));
    let welcome = gtk::Box::new(gtk::Orientation::Vertical, 0);
    welcome.append(&omui::header(None));
    welcome.append(&welcome_page.widget);
    let anchor = welcome_page.widget.clone();
    i18n::on_change(&anchor, move || {
        welcome_page.set_title(&tr("Your Contacts"));
        welcome_page.set_description(&tr(
            "Link your iCloud or Google account to see its contacts here. Each account keeps its own.",
        ));
    });
    let stack = gtk::Stack::new();
    stack.add_named(&body, Some("contacts"));
    stack.add_named(&welcome, Some("welcome"));

    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .icon_name(super::NAME)
        .default_width(900)
        .default_height(640)
        .width_request(360)
        .height_request(320)
        .build();
    // The header bars are the panes' own, in the content.
    omui::claim_titlebar(&window);
    i18n::bind(&window, |window| window.set_title(Some(&tr("Contacts"))));
    let shell = omui::Shell::new(&window, &stack);

    Window {
        window,
        shell,
        banner,
        search,
        list,
        list_empty,
        card,
        card_stack,
        split,
        edit_button,
        delete_button,
        filter,
        filter_names,
        accounts_button: accounts_button.clone(),
        stack,
        transport: Arc::default(),
        root: Store::default_root(),
        accounts: RefCell::default(),
        shown: RefCell::default(),
        selected: RefCell::default(),
        visible: RefCell::default(),
        syncing: Cell::new(false),
        last_error: RefCell::default(),
        ks_offer,
        ks_list,
        ks_row,
        ks_switch,
        ks_shown: Cell::new(false),
        dup_offer,
        editing: RefCell::default(),
        asking: Cell::new(false),
        main_menu,
        no_card,
        shortcuts: omui::ShortcutOverlay::new(shortcut_sections),
        mirror_line: RefCell::default(),
        duplicates: Cell::new(0),
    }
}

/// The book of `config`'s accounts over this window's transport.
fn open(
    config: &config::Config,
    root: Option<PathBuf>,
    t: Arc<SessionTransport>,
) -> cloud_contacts::error::Result<Book> {
    let root = root.ok_or_else(|| Error::Other(tr("no data directory ($HOME is not set)")))?;
    Book::open(config, root, t)
}

/// The accounts' names and providers.
fn names(book: &Book) -> Vec<(String, String)> {
    book.accounts
        .iter()
        .map(|a| (a.name.clone(), a.provider.clone()))
        .collect()
}

/// The sidebar menu's items, in the language of the moment.
fn main_menu_items(menu: &gio::Menu) {
    menu.remove_all();
    menu.append(Some(&tr("Refresh")), Some("win.refresh"));
    menu.append(Some(&tr("Duplicates…")), Some("win.duplicates"));
}

/// Every shortcut, for the overlay, keys that do the same on one row as separate key caps; README.md's Keyboard table
/// lists the same.
fn shortcut_sections() -> Vec<omui::ShortcutSection> {
    vec![
        omui::ShortcutSection::new(
            &tr("Contacts"),
            &[
                ("Ctrl+N", &tr("New contact")),
                ("Ctrl+E", &tr("Edit the chosen contact")),
                ("Ctrl+F", &tr("Search")),
                ("Enter", &tr("In the search, open the first match")),
            ],
        )
        .alternatives(&["Up", "Down"], &tr("The contact above or below in the list"))
        .alternatives(&["Ctrl+R", "F5"], &tr("Fetch every account now"))
        .alternatives(
            &["Escape", "Alt+Left"],
            &tr("In a narrow window, back from a contact to the list"),
        ),
        omui::ShortcutSection::new(&tr("Window"), &[])
            .alternatives(&["F1", "Ctrl+?"], &tr("Show or hide the shortcuts"))
            .alternatives(&["Ctrl+W", "Ctrl+Q"], &tr("Close the window")),
        omui::ShortcutSection::new(
            &tr("Dialogs"),
            &[
                ("Enter", &tr("Save, or the dialog's default answer")),
                ("Escape", &tr("Cancel; with something typed, it asks first")),
                ("Alt+S", &tr("Save")),
                ("Alt+C", &tr("Cancel")),
            ],
        )
        .alternatives(&["Ctrl+Z", "Ctrl+Shift+Z"], &tr("Undo and redo in a field")),
    ]
}

/// The edit dialog's title for one of its single-line fields.
fn field_title(field: Field) -> String {
    match field {
        Field::First => tr("First name"),
        Field::Last => tr("Last name"),
        Field::Company => tr("Company"),
        Field::JobTitle => tr("Job title"),
        Field::Nickname => tr("Nickname"),
        Field::Birthday => tr("Birthday (1990-05-01, or 05-01)"),
        // Not among the dialog's fields (`SCALARS`); named as the library names it.
        other => SCALARS
            .iter()
            .find(|(f, _)| *f == other)
            .map(|(_, title)| i18n::translate(title))
            .unwrap_or_default(),
    }
}

/// A phone's, email's, URL's or address's label as the window shows it: the standard ones iCloud stores in capitals
/// in the person's language, and one the person typed as they typed it.
fn label_title(label: &str) -> String {
    match label {
        "" | "OTHER" => tr("other"),
        "MOBILE" => tr("mobile"),
        "HOME" => tr("home"),
        "WORK" => tr("work"),
        "MAIN" => tr("main"),
        "PAGER" => tr("pager"),
        "HOME FAX" => tr("home fax"),
        "WORK FAX" => tr("work fax"),
        "HOMEPAGE" => tr("homepage"),
        "SCHOOL" => tr("school"),
        _ => model::label_display(label),
    }
}

/// Why Save is held back, in the person's language.
fn problem_text(problem: &Problem) -> String {
    match problem {
        Problem::Birthday(typed) => tr!("birthday \"{}\": use YYYY-MM-DD, or MM-DD without a year", typed),
        Problem::Empty(Multi::Phone) => tr("an empty phone number"),
        Problem::Empty(Multi::Email) => tr("an empty email address"),
        Problem::Empty(Multi::Url) => tr("an empty URL"),
        Problem::NotPhone(value) => tr!("\"{}\" is not a phone number", value),
        Problem::NotEmail(value) => tr!("\"{}\" is not an email address", value),
        Problem::Nameless => tr("A contact needs a name, a company, a phone number or an email."),
    }
}

/// The line under the mirroring switch, in the person's language.
fn mirror_text(line: &mirror::Line) -> String {
    match line {
        mirror::Line::SignedOut(who) => tr!("{} is signed out: sign in again for changes to cross", who),
        mirror::Line::Off => tr("Off"),
        // What stopped a run, as the library recorded it.
        mirror::Line::Problem(problem) => problem.clone(),
        mirror::Line::Waiting(n) => trn!("{n} possible duplicates wait for you", *n),
        mirror::Line::LastRun(when) => {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            match now.saturating_sub(*when) {
                0..60 => tr("On · last mirrored just now"),
                s @ 60..3600 => trn!("On · last mirrored {n} minutes ago", s / 60),
                s @ 3600..86_400 => trn!("On · last mirrored {n} hours ago", s / 3600),
                s => trn!("On · last mirrored {n} days ago", s / 86_400),
            }
        }
        mirror::Line::On => tr("On"),
    }
}

/// A stored birthday (`1990-05-01`, or `1604-05-01` without a year) as the person's language writes a date: its own
/// short date, or the day and the month's name without a year. One that is not a date shows as stored.
fn birthday_text(stored: &str) -> String {
    let parts: Vec<i32> = stored.split('-').filter_map(|p| p.parse().ok()).collect();
    let [year, month, day] = parts[..] else {
        return model::birthday_display(stored);
    };
    match glib::DateTime::from_local(year, month, day, 0, 0, 0.0) {
        Ok(date) if year == 1604 => i18n::format_day_month(&date),
        Ok(date) => i18n::format_date(&date),
        Err(_) => model::birthday_display(stored),
    }
}

/// Asks before something that cannot be taken back, as [`omui::confirm`] does with a destructive action (Enter and
/// Escape cancel), for a heading with a name in it, which `confirm`'s English-in, translated-out text cannot be:
/// `text` writes the heading, the body and the `confirm` button's label now and after each switch of language.
fn confirm_destructive(
    parent: &impl IsA<gtk::Widget>,
    text: impl Fn(&omui::Dialog) + 'static,
    yes: impl FnOnce() + 'static,
) {
    let dialog = omui::Dialog::new(Some(""), Some(""));
    dialog.add_response("cancel", "");
    dialog.add_response("confirm", "");
    dialog.set_response_translated("cancel", "_Cancel");
    dialog.bind(text);
    dialog.set_response_appearance("confirm", omui::Appearance::Destructive);
    dialog.set_default_response("cancel");
    dialog.set_close_response("cancel");
    let yes = Cell::new(Some(yes));
    dialog.connect_response(move |_, id| {
        if id == "confirm"
            && let Some(yes) = yes.take()
        {
            yes();
        }
    });
    dialog.present(parent);
}
