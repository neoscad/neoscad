//! One error type: a sentence for the user, which says what to do where it
//! can.

use std::fmt;
use std::path::Path;

/// A failed project operation, as the message the command line prints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub String);

pub type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl Error {
    pub fn new(msg: impl Into<String>) -> Error {
        Error(msg.into())
    }
}

/// `Err(Error)` from a format string.
macro_rules! bail {
    ($($t:tt)*) => {
        return Err($crate::error::Error(format!($($t)*)))
    };
}
pub(crate) use bail;

/// Adds what was being done to an I/O error.
pub(crate) trait Context<T> {
    fn ctx(self, what: impl FnOnce() -> String) -> Result<T>;
    /// "cannot VERB PATH: error".
    fn at(self, verb: &str, path: &Path) -> Result<T>
    where
        Self: Sized,
    {
        self.ctx(|| format!("cannot {verb} {}", path.display()))
    }
}

impl<T> Context<T> for std::io::Result<T> {
    fn ctx(self, what: impl FnOnce() -> String) -> Result<T> {
        self.map_err(|e| Error(format!("{}: {e}", what())))
    }
}
