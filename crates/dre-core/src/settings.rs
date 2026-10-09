//! DRE's settings and where each value came from.
//!
//! A setting is chosen, highest first, by its command-line flag, its `DRE_*` environment
//! variable, its key in `dre_project.yml`, then its default. [`env`] is the one place DRE reads
//! its own environment variables; [`Settings`] records every resolved value with its source, for
//! `dre validate --verbose` (and its JSON), `run_results.json` and the run's log.

use std::fmt;

use serde::Serialize;

/// The run's target: `--target`, `DRE_TARGET`.
pub const TARGET: &str = "DRE_TARGET";
/// The target path: `--target-path`, `DRE_TARGET_PATH`, `target_path:`.
pub const TARGET_PATH: &str = "DRE_TARGET_PATH";
/// Where profiles.yml is: `--profiles-dir`, `DRE_PROFILES_DIR`.
pub const PROFILES_DIR: &str = "DRE_PROFILES_DIR";
/// Where plugin packages are installed: `DRE_PLUGINS_DIR`.
pub const PLUGINS_DIR: &str = "DRE_PLUGINS_DIR";
/// The plugin registry index: `DRE_REGISTRY_URL`.
pub const REGISTRY_URL: &str = "DRE_REGISTRY_URL";
/// The GitHub API DRE asks for releases (tests point it elsewhere): `DRE_GITHUB_API_URL`.
pub const GITHUB_API_URL: &str = "DRE_GITHUB_API_URL";
/// The run's timezone: `--timezone`, `DRE_TIMEZONE`.
pub const TIMEZONE: &str = "DRE_TIMEZONE";
/// `run.date`: `DRE_RUN_DATE`.
pub const RUN_DATE: &str = "DRE_RUN_DATE";
/// The instant a scheduled run was scheduled for: `DRE_RUN_AT`.
pub const RUN_AT: &str = "DRE_RUN_AT";
/// How many lines `dre.log` keeps: `DRE_LOG_MAX_LINES`.
pub const LOG_MAX_LINES: &str = "DRE_LOG_MAX_LINES";

/// A `DRE_*` environment variable's value; set but empty counts as unset.
pub fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// Where a setting's value came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A command-line flag, e.g. `--target`.
    Flag(&'static str),
    /// An environment variable, e.g. `DRE_TARGET`.
    Env(&'static str),
    /// A key of `dre_project.yml`.
    Project(&'static str),
    /// Found by looking, e.g. profiles.yml in the project directory.
    Found(&'static str),
    /// Its default.
    Default,
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Source::Flag(name) | Source::Env(name) | Source::Found(name) => f.write_str(name),
            Source::Project(key) => write!(f, "`{key}` in dre_project.yml"),
            Source::Default => f.write_str("default"),
        }
    }
}

impl Serialize for Source {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

/// One resolved setting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Setting {
    pub name: &'static str,
    /// The value as text; `None` when it's unset and has no default.
    pub value: Option<String>,
    pub source: Source,
}

/// Every resolved setting, in the order they were resolved.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Settings(Vec<Setting>);

impl Settings {
    /// Record a setting, replacing an earlier value of the same name.
    pub fn set(&mut self, name: &'static str, value: Option<String>, source: Source) {
        self.0.retain(|s| s.name != name);
        self.0.push(Setting { name, value, source });
    }

    pub fn get(&self, name: &str) -> Option<&Setting> {
        self.0.iter().find(|s| s.name == name)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Setting> {
        self.0.iter()
    }

    /// One line per setting, `name = value (source)`, for the log and `validate --verbose`.
    pub fn lines(&self) -> Vec<String> {
        self.0
            .iter()
            .map(|s| {
                format!(
                    "{} = {} ({})",
                    s.name,
                    s.value.as_deref().unwrap_or("unset"),
                    s.source
                )
            })
            .collect()
    }
}

/// The settings a command line resolves before loading a project: the run's timezone
/// (`--timezone`, `DRE_TIMEZONE`), `run.date` (`DRE_RUN_DATE`) and the scheduled instant
/// (`DRE_RUN_AT`).
pub fn run_settings(timezone_flag: Option<&str>) -> Settings {
    let mut s = Settings::default();
    let (tz, src) = match flag_or_env(timezone_flag, "--timezone", TIMEZONE) {
        Some((v, src)) => (Some(v), src),
        None => (None, Source::Default),
    };
    s.set("timezone", tz, src);
    for (name, var) in [("run_date", RUN_DATE), ("scheduled_at", RUN_AT)] {
        match env(var) {
            Some(v) => s.set(name, Some(v), Source::Env(var)),
            None => s.set(name, None, Source::Default),
        }
    }
    s
}

/// The value of a setting given by `flag`, else the environment variable `var`, with its source.
pub fn flag_or_env(
    flag: Option<&str>,
    flag_name: &'static str,
    var: &'static str,
) -> Option<(String, Source)> {
    match flag.filter(|f| !f.is_empty()) {
        Some(f) => Some((f.to_string(), Source::Flag(flag_name))),
        None => env(var).map(|v| (v, Source::Env(var))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flag_beats_the_environment() {
        assert_eq!(
            flag_or_env(Some("prod"), "--target", "DRE_TEST_SETTINGS_UNSET"),
            Some(("prod".into(), Source::Flag("--target")))
        );
        assert_eq!(flag_or_env(None, "--target", "DRE_TEST_SETTINGS_UNSET"), None);
        assert_eq!(flag_or_env(Some(""), "--target", "DRE_TEST_SETTINGS_UNSET"), None);
    }

    #[test]
    fn settings_record_their_source_and_replace_earlier_values() {
        let mut s = Settings::default();
        s.set("target", Some("dev".into()), Source::Default);
        s.set("target_path", None, Source::Default);
        s.set("target", Some("prod".into()), Source::Env(TARGET));
        assert_eq!(
            s.lines(),
            ["target_path = unset (default)", "target = prod (DRE_TARGET)"]
        );
        assert_eq!(
            serde_json::to_value(&s).unwrap()[1],
            serde_json::json!({"name": "target", "value": "prod", "source": "DRE_TARGET"})
        );
    }
}
