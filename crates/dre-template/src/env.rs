//! The process environment, read once and passed around.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

/// A snapshot of the environment variables a command sees, and the user's home directory.
#[derive(Clone, Default, Debug)]
pub struct Env {
    vars: Arc<BTreeMap<String, String>>,
    home: Option<PathBuf>,
}

impl Env {
    /// The process's environment, read once. Only an entry point (the CLI's `main`) calls this.
    pub fn from_process() -> Env {
        Env {
            vars: Arc::new(
                std::env::vars_os()
                    .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
                    .collect(),
            ),
            home: std::env::home_dir(),
        }
    }

    /// An environment of exactly `vars`, with no home directory (tests, embedding).
    pub fn from_vars<K: Into<String>, V: Into<String>>(vars: impl IntoIterator<Item = (K, V)>) -> Env {
        Env {
            vars: Arc::new(vars.into_iter().map(|(k, v)| (k.into(), v.into())).collect()),
            home: None,
        }
    }

    /// This environment with `home` as the home directory.
    pub fn with_home(mut self, home: PathBuf) -> Env {
        self.home = Some(home);
        self
    }

    /// A variable's value as set, empty included.
    pub fn var(&self, name: &str) -> Option<&str> {
        self.vars.get(name).map(String::as_str)
    }

    /// A `DRE_*` setting's value; set but empty counts as unset.
    pub fn setting(&self, name: &str) -> Option<String> {
        self.var(name).filter(|v| !v.is_empty()).map(str::to_string)
    }

    /// Every variable, in name order.
    pub fn vars(&self) -> impl Iterator<Item = (&str, &str)> {
        self.vars.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    /// The user's home directory (empty when unknown).
    pub fn home(&self) -> PathBuf {
        self.home.clone().unwrap_or_default()
    }

    /// DRE's home directory: `~/.dre`.
    pub fn dre_home(&self) -> PathBuf {
        self.home().join(".dre")
    }
}
