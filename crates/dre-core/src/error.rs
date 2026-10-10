//! DRE's error type at module boundaries: a registered [`Code`] (whose [`Kind`] decides the exit
//! code and whether trying again can help), the message, and an optional hint at the fix.

use crate::codes::{Code, ErrorCode, Kind};

/// A failure with its code.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct Error {
    pub code: ErrorCode,
    pub message: String,
    /// What to do about it, when the message doesn't already say.
    pub hint: Option<String>,
}

impl Error {
    pub fn new(code: impl Into<ErrorCode>, message: impl Into<String>) -> Error {
        Error {
            code: code.into(),
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
            self.code = code.into();
        }
        self
    }

    /// A plugin's failure: its own code when it gave one (`code` without `kind` counts as
    /// `internal`), else `run-failed` until the step gives it one. `message` is what to show.
    pub fn from_plugin(e: &dre_protocol::host::HostError, message: impl Into<String>) -> Error {
        let code = match e {
            dre_protocol::host::HostError::Plugin {
                code: Some(code),
                kind,
                ..
            } => ErrorCode::Plugin {
                code: code.clone(),
                kind: kind.as_deref().and_then(Kind::parse).unwrap_or(Kind::Internal),
            },
            _ => ErrorCode::Core(Code::RunFailed),
        };
        Error {
            code,
            message: message.into(),
            hint: None,
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use dre_protocol::host::HostError;

    fn plugin(kind: Option<&str>, code: Option<&str>) -> HostError {
        HostError::Plugin {
            plugin: "dre-plugin-sftp".into(),
            message: "refused".into(),
            kind: kind.map(str::to_string),
            code: code.map(str::to_string),
        }
    }

    #[test]
    fn a_plugin_code_is_kept_with_its_kind() {
        let e = Error::from_plugin(&plugin(Some("auth"), Some("sftp/bad-key")), "m").or(Code::DeliveryFailed);
        assert_eq!(e.code.as_str(), "sftp/bad-key");
        assert_eq!(e.kind(), Kind::Auth);
        // A code without a kind counts as internal.
        let e = Error::from_plugin(&plugin(None, Some("sftp/odd")), "m");
        assert_eq!(e.kind(), Kind::Internal);
    }

    #[test]
    fn without_a_code_the_step_gives_its_own() {
        let e = Error::from_plugin(&plugin(Some("auth"), None), "m").or(Code::DeliveryFailed);
        assert_eq!(e.code, Code::DeliveryFailed);
    }
}
