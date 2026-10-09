//! DRE's error type at module boundaries: a registered [`Code`] (whose [`Kind`] decides the exit
//! code and whether trying again can help), the message, and an optional hint at the fix.

use crate::codes::{Code, Kind};

/// A failure with its code.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct Error {
    pub code: Code,
    pub message: String,
    /// What to do about it, when the message doesn't already say.
    pub hint: Option<String>,
}

impl Error {
    pub fn new(code: Code, message: impl Into<String>) -> Error {
        Error {
            code,
            message: message.into(),
            hint: None,
        }
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Error {
        self.hint = Some(hint.into());
        self
    }

    pub fn kind(&self) -> Kind {
        self.code.kind()
    }

    /// The step's own code, unless a more specific one is already set (anything but
    /// `run-failed`, what an uncoded message gets).
    pub fn or(mut self, code: Code) -> Error {
        if self.code == Code::RunFailed {
            self.code = code;
        }
        self
    }
}

/// An uncoded message: `run-failed`, until the step that knows better gives it a code.
impl From<String> for Error {
    fn from(message: String) -> Error {
        Error::new(Code::RunFailed, message)
    }
}

impl From<&str> for Error {
    fn from(message: &str) -> Error {
        Error::new(Code::RunFailed, message)
    }
}
