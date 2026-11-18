//! One error for every account: iCloud's (whatever icloud-session failed
//! with, or a card iCloud refused), Google's (what `gws` answered), and the
//! accounts' own configuration. The command line turns each into the shared
//! contract's code and exit code (`cli`); the window asks [`Error::is_sign_in`]
//! and [`Error::is_conflict`].

/// What kind of trouble a Google account is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Signed out, or the sign-in expired or was revoked.
    Auth,
    /// `gws` is missing, timed out, or Google could not be reached.
    Unavailable,
    NotFound,
    /// Google refused the request as invalid.
    BadRequest,
    /// The contact changed elsewhere since it was read (its etag is stale).
    Conflict,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Anything icloud-session failed with: signed out, offline, an HTTP
    /// status, the daemon or the keyring out of reach, an answer that is not
    /// JSON. Kept whole, so the command line reports it as every iCloud
    /// tool does.
    #[error(transparent)]
    Session(#[from] icloud_session::Error),
    /// iCloud answered 200 but refused the card (412: changed elsewhere).
    #[error("iCloud refused the change ({code}): {message}")]
    Refused { code: i64, message: String },
    /// A Google account's failure; `account` is its name.
    #[error("{message}")]
    Google {
        account: String,
        kind: Kind,
        message: String,
    },
    /// No account linked, an unknown account, or a config that does not read.
    #[error("{0}")]
    Config(String),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// The account needs signing in (again): the window shows its banner.
    pub fn is_sign_in(&self) -> bool {
        matches!(
            self,
            Error::Session(icloud_session::Error::SignInRequired) | Error::Google { kind: Kind::Auth, .. }
        )
    }

    /// iCloud's sign-in in particular (icloud-session's banner can fix it).
    pub fn is_icloud_sign_in(&self) -> bool {
        matches!(self, Error::Session(icloud_session::Error::SignInRequired))
    }

    /// The card changed on another device since its `etag` was read.
    pub fn is_conflict(&self) -> bool {
        matches!(
            self,
            Error::Refused { code: 412, .. }
                | Error::Google {
                    kind: Kind::Conflict,
                    ..
                }
        )
    }

    /// iCloud no longer accepts the tokens the request carried.
    pub fn is_stale_token(&self) -> bool {
        matches!(self, Error::Session(icloud_session::Error::Http { status: 420, .. }))
    }

    pub fn is_not_found(&self) -> bool {
        matches!(
            self,
            Error::Google {
                kind: Kind::NotFound,
                ..
            }
        )
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Other(e.to_string())
    }
}
