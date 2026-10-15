"""Compact native chrome; GTK resolves colors from the current Omarchy theme."""
import os
from pathlib import Path
import tomllib
from gi.repository import Adw, Gdk, Gio, GLib, Gtk

CSS = """
/* Compact chrome follows the active GTK/Omarchy palette. */
.cloud-desktop { font-family: monospace; font-size: 10pt; background: @window_bg_color; color: @window_fg_color; }
.cloud-desktop headerbar { min-height: 46px; padding: 4px 12px; background: @window_bg_color; box-shadow: none; border-bottom: 1px solid alpha(@window_fg_color, 0.12); }
.cloud-desktop button { min-height: 28px; border-radius: 6px; box-shadow: none; font-weight: 500; }
.cloud-desktop button:not(.suggested-action):not(.destructive-action) { background: transparent; border-color: transparent; }
.cloud-desktop button:hover { background: alpha(@window_fg_color, 0.07); }
.cloud-desktop button:focus-visible { outline: 1px solid @accent_bg_color; outline-offset: 2px; }
.cloud-desktop button.suggested-action { background: alpha(@accent_bg_color, 0.14); color: @accent_color; border: 1px solid alpha(@accent_bg_color, 0.35); }
.cloud-desktop entry, .cloud-desktop searchentry { min-height: 34px; border-radius: 6px; background: alpha(@window_fg_color, 0.025); border: 1px solid alpha(@window_fg_color, 0.16); box-shadow: none; }
.cloud-desktop entry:focus-within, .cloud-desktop searchentry:focus-within { border-color: @accent_bg_color; outline: none; }
.cloud-desktop .record-list { background: transparent; border: none; box-shadow: none; }
.cloud-desktop .record-list > row { border-radius: 6px; margin: 2px 12px; border: 1px solid transparent; background: transparent; }
.cloud-desktop .record-list > row:hover { background: alpha(@window_fg_color, 0.04); }
.cloud-desktop .record-list > row:selected { background: alpha(@accent_bg_color, 0.12); border-left-color: @accent_bg_color; color: @window_fg_color; }
.cloud-desktop .record-title { font-weight: 600; }
.cloud-desktop .record-icon { color: @accent_color; padding: 10px; border-radius: 6px; background: alpha(@accent_bg_color, 0.06); }
.cloud-desktop .record-detail { font-size: 9pt; opacity: 0.65; }
.cloud-desktop .record-tools { padding: 8px 16px; border-bottom: 1px solid alpha(@window_fg_color, 0.08); }
.cloud-desktop .record-notice { font-size: 9pt; opacity: 0.75; }
.cloud-desktop scrollbar slider { min-width: 4px; min-height: 4px; border-radius: 4px; }
"""


_provider = None
_monitors = []
_last = None


def current():
    return Path(os.environ.get('XDG_STATE_HOME', str(Path.home() / '.local/state'))) / 'omarchy/current'


def palette():
    try:
        return tomllib.loads((current() / 'theme/colors.toml').read_text())
    except (OSError, ValueError):
        return {}


def refresh():
    global _last
    colors = palette()
    if colors == _last:
        return False
    _last = colors
    css, properties = CSS, []
    roles = {
        'background': ['window_bg_color', 'view_bg_color', 'headerbar_bg_color', 'card_bg_color', 'popover_bg_color', 'sidebar_bg_color', 'secondary_sidebar_bg_color', 'dialog_bg_color'],
        'foreground': ['window_fg_color', 'view_fg_color', 'headerbar_fg_color', 'card_fg_color', 'popover_fg_color', 'sidebar_fg_color', 'secondary_sidebar_fg_color', 'dialog_fg_color'],
        'accent': ['accent_bg_color', 'accent_color'],
    }
    for key, names in roles.items():
        color = colors.get(key)
        if not isinstance(color, str) or not color.startswith('#') or not Gdk.RGBA().parse(color):
            continue
        for name in names:
            css += f'\n@define-color {name} {color};'
            properties.append(f'--{name.replace("_", "-")}: {color};')
    css += '\n.cloud-desktop { ' + ''.join(properties) + ' }'
    _provider.load_from_string(css)
    scheme = {'dark': Adw.ColorScheme.PREFER_DARK, 'light': Adw.ColorScheme.PREFER_LIGHT}
    Adw.StyleManager.get_default().set_color_scheme(scheme.get(colors.get('mode'), Adw.ColorScheme.DEFAULT))
    return False


def install():
    global _provider
    display = Gdk.Display.get_default()
    if display is None or _provider is not None:
        return
    _provider = Gtk.CssProvider()
    Gtk.StyleContext.add_provider_for_display(display, _provider, Gtk.STYLE_PROVIDER_PRIORITY_USER)
    refresh()
    # current/ survives Omarchy replacing the theme directory by rename.
    for path in [current(), current() / 'theme']:
        try:
            monitor = Gio.File.new_for_path(str(path)).monitor_directory(Gio.FileMonitorFlags.WATCH_MOVES, None)
        except GLib.Error:
            continue
        monitor.connect('changed', lambda *_: GLib.timeout_add(300, refresh))
        _monitors.append(monitor)
