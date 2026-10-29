//! What one command runs with: its environment, its secrets, its plugin host and the answers it
//! has cached. Made once per command (per run) and passed in, so two runs in one process share
//! nothing: no secret, no cache and no cancellation.
//!
//! [`Env::from_process`] is the one place core reads the process environment; everything else
//! reads the [`Env`] it's given.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use crate::plugin_host::{Described, PluginHost, SubprocessHost};
use crate::profiles::Role;
use crate::secrets::Secrets;

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

/// `(project root, role, plugin type)`: what a `describe` answer is cached by.
pub(crate) type DescribeKey = (PathBuf, Role, String);

/// `(project, source.table, connection, target)`: a source table whose columns were checked.
pub(crate) type CheckedKey = (PathBuf, String, String, String);

/// One command's environment, secrets, plugin host and caches. Cloning shares them: every
/// thread of a run uses the same context.
#[derive(Clone)]
pub struct Context(Arc<Inner>);

struct Inner {
    env: Env,
    secrets: Secrets,
    host: Arc<dyn PluginHost>,
    /// What each plugin type's `describe` said (`None`: it couldn't be asked), asked once.
    described: Mutex<BTreeMap<DescribeKey, Option<Described>>>,
    /// Source tables whose declared columns `validate --live` already checked.
    checked: Mutex<BTreeSet<CheckedKey>>,
    /// `flags: http_timeout` of the project being run, once it's loaded.
    http_timeout: OnceLock<u64>,
}

impl Context {
    /// A context over `env`, starting plugins through `host`, masking `env`'s `DRE_SECRET_*`.
    pub fn new(env: Env, host: Arc<dyn PluginHost>) -> Context {
        let secrets = Secrets::new(&env);
        Context::with_secrets(env, secrets, host)
    }

    /// A context sharing `secrets` with whatever else holds them (the CLI's console).
    pub fn with_secrets(env: Env, secrets: Secrets, host: Arc<dyn PluginHost>) -> Context {
        Context(Arc::new(Inner {
            env,
            secrets,
            host,
            described: Mutex::default(),
            checked: Mutex::default(),
            http_timeout: OnceLock::new(),
        }))
    }

    /// No environment variables, and plugins run as child processes.
    pub fn empty() -> Context {
        Context::subprocess(Env::default())
    }

    /// A context over `env` that runs plugins as child processes.
    pub fn subprocess(env: Env) -> Context {
        let host = Arc::new(SubprocessHost::new(&env));
        Context::new(env, host)
    }

    /// The command's environment variables.
    pub fn env(&self) -> &Env {
        &self.0.env
    }

    /// The values the command masks.
    pub fn secrets(&self) -> &Secrets {
        &self.0.secrets
    }

    /// What finds, starts and stops the command's plugins.
    pub fn host(&self) -> &Arc<dyn PluginHost> {
        &self.0.host
    }

    /// `describe`'s cached answer for `key`, or `ask`'s, cached.
    pub(crate) fn described(
        &self,
        key: DescribeKey,
        ask: impl FnOnce() -> Option<Described>,
    ) -> Option<Described> {
        if let Some(v) = self.0.described.lock().unwrap().get(&key) {
            return v.clone();
        }
        let asked = ask();
        self.0.described.lock().unwrap().insert(key, asked.clone());
        asked
    }

    /// Whether `key` is checked for the first time (it's then recorded).
    pub(crate) fn first_check(&self, key: CheckedKey) -> bool {
        self.0.checked.lock().unwrap().insert(key)
    }

    /// `flags: http_timeout` from the project being run (set once, when it loads).
    pub fn set_project_http_timeout(&self, secs: u64) {
        let _ = self.0.http_timeout.set(secs);
    }

    /// How long a download may receive nothing: `DRE_HTTP_TIMEOUT`, else the project's
    /// `flags: http_timeout`, else 60 seconds.
    pub fn http_timeout(&self) -> std::time::Duration {
        let env = self
            .env()
            .setting(crate::settings::HTTP_TIMEOUT)
            .and_then(|v| v.trim().parse::<u64>().ok());
        std::time::Duration::from_secs(
            env.or_else(|| self.0.http_timeout.get().copied())
                .filter(|s| *s > 0)
                .unwrap_or(crate::manager::DEFAULT_HTTP_TIMEOUT),
        )
    }
}

impl std::fmt::Debug for Context {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Context").finish_non_exhaustive()
    }
}
