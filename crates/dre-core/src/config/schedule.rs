//! The shapes of `sets.yml` (Sets shared by reports), `timings.yml` (named timings) and
//! `schedules.yml` (named schedules).
//!
//! A timing block (`cron`, `every`, `rrule` and their `starting`, `at`, `except`, `also`) is
//! checked by [`crate::schedule::validate_block`]; here it's read as written.

use schemars::JsonSchema;
use serde::Deserialize;

use super::de::{Located, Loose, Map, UnknownKeys};

type JsonMap = serde_json::Map<String, serde_json::Value>;
/// A timing block's key, as written, with where (to keep the file's order).
type Raw = Option<Located<serde_json::Value>>;

/// Set names mapped to their definitions.
#[derive(Deserialize, JsonSchema)]
#[serde(transparent)]
#[schemars(title = "DRE Sets")]
pub struct SetsFile(pub Map<Loose<Option<SetConfig>>>);

/// One Set.
#[derive(Deserialize, JsonSchema)]
#[schemars(rename = "set", deny_unknown_fields)]
pub struct SetConfig {
    /// The connection a Binding with this Set runs on. May use Jinja with `var()`, `env_var()`, `run.*` and `target.name`.
    pub profile: Option<Loose<String>>,
    /// Variables, read in SQL and YAML with `var('name')`. Values can be strings, numbers, booleans, lists or maps.
    #[serde(default)]
    #[schemars(schema_with = "any_map")]
    pub vars: Option<Loose<JsonMap>>,
    /// The locale for this Set's number filters (`fr-FR`), above the report's.
    pub locale: Option<Loose<String>>,
}

/// The timings file, `timings.yml`: timing names mapped to timings that any schedule can use with `timing: <name>`.
#[derive(Deserialize, JsonSchema)]
#[serde(transparent)]
#[schemars(title = "DRE timings", extend("propertyNames" = {"pattern": "^[A-Za-z_][A-Za-z0-9_]*$"}))]
pub struct TimingsFile(pub Map<Loose<TimingConfig>>);

/// One named timing: when it fires, in which timezone, and the dates it skips or adds.
#[derive(Deserialize, JsonSchema)]
#[schemars(rename = "timing", deny_unknown_fields)]
pub struct TimingConfig {
    /// A cron expression (5 fields, or a macro such as `@daily`). A timing needs exactly one of `cron`, `every` or `rrule`.
    #[serde(default)]
    #[schemars(with = "String")]
    pub cron: Raw,
    /// Every N days, weeks or months, with exactly one unit, e.g. `{days: 3}`.
    #[serde(default)]
    #[schemars(with = "Every")]
    pub every: Raw,
    /// An iCalendar recurrence rule, e.g. `FREQ=MONTHLY;BYDAY=2TU`. Use `starting` for a rule with `INTERVAL` above 1 or `COUNT`, and `at` for its time of day.
    #[serde(default)]
    #[schemars(with = "String")]
    pub rrule: Raw,
    /// With `every` or `rrule`: the first date, `YYYY-MM-DD`. `every` needs it, and so does a rule with `INTERVAL` above 1, a `COUNT`, or a day it takes from its start.
    #[serde(default)]
    #[schemars(with = "String", regex(pattern = "^[0-9]{4}-[0-9]{2}-[0-9]{2}$"))]
    pub starting: Raw,
    /// With `every` or `rrule`: the time of day, `HH:MM` (24-hour), unless the rule sets `BYHOUR`/`BYMINUTE`. Default: 00:00.
    #[serde(default)]
    #[schemars(with = "String", regex(pattern = "^[0-9]{2}:[0-9]{2}$"))]
    pub at: Raw,
    /// Dates (`YYYY-MM-DD`, in its timezone) it doesn't fire on, e.g. holidays.
    #[serde(default)]
    #[schemars(with = "Vec<Date>")]
    pub except: Raw,
    /// Extra dates (`YYYY-MM-DD`) it fires on, at its time of day in its timezone. Needs a timing that fires at one time of day.
    #[serde(default)]
    #[schemars(with = "Vec<Date>")]
    pub also: Raw,
    /// The timezone it fires in, an IANA name such as `Australia/Sydney`; schedules using the timing also run in it. Default: the project's `timezone:`, then UTC.
    pub timezone: Option<Loose<String>>,
    #[serde(rename = "$unknown", default)]
    #[schemars(skip)]
    pub unknown: UnknownKeys,
}

impl TimingConfig {
    /// The timing block as written, for [`crate::schedule::validate_block`].
    pub fn block(&self) -> JsonMap {
        block([
            ("cron", &self.cron),
            ("every", &self.every),
            ("rrule", &self.rrule),
            ("starting", &self.starting),
            ("at", &self.at),
            ("except", &self.except),
            ("also", &self.also),
        ])
    }
}

/// The schedules file, `schedules.yml`: a list of named schedules.
#[derive(Deserialize, JsonSchema)]
#[serde(transparent)]
#[schemars(title = "DRE schedules")]
pub struct SchedulesFile(pub Vec<Located<Loose<ScheduleConfig>>>);

/// One named schedule. DRE doesn't fire it; your orchestrator runs `dre run --schedule <name>`.
#[derive(Deserialize, JsonSchema)]
#[schemars(rename = "schedule", deny_unknown_fields)]
pub struct ScheduleConfig {
    /// The schedule's name: letters, digits and `_`, not starting with a digit. Unique across the project.
    #[schemars(required, regex(pattern = "^[A-Za-z_][A-Za-z0-9_]*$"))]
    pub name: Option<Loose<String>>,
    /// The report to run. Use `report` or `select`, not both.
    pub report: Option<Loose<String>>,
    /// The Set of `report` to run. Only with `report`.
    pub set: Option<Loose<String>>,
    /// A selector for the reports to run, e.g. `tag:regulatory`. Use `report` or `select`, not both.
    pub select: Option<Loose<String>>,
    /// Variables for the run: above the report's own and below `--var`.
    #[serde(default)]
    #[schemars(schema_with = "any_map")]
    pub vars: Option<Loose<JsonMap>>,
    /// The name of a timing in `timings.yml` to fire on. Use `timing` or one of `cron`, `every`, `rrule`; with `timing`, the schedule sets none of the timing's keys (`timezone`, `starting`, `at`, `except`, `also`).
    #[schemars(regex(pattern = "^[A-Za-z_][A-Za-z0-9_]*$"))]
    pub timing: Option<Loose<String>>,
    /// The timezone it fires in, and the one `run.date` and `run.now` use, an IANA name such as `Australia/Sydney`. Default: the project's `timezone:` for firing (the report's for the run), then UTC.
    pub timezone: Option<Located<Loose<String>>>,
    /// `false` pauses the schedule: it keeps its name and settings but `dre schedule ls` lists no occurrences for it. Default: `true`.
    pub enabled: Option<Loose<bool>>,
    /// A cron expression (5 fields, or a macro such as `@daily`). A schedule needs exactly one of `timing`, `cron`, `every` or `rrule`.
    #[serde(default)]
    #[schemars(with = "String")]
    pub cron: Raw,
    /// Every N days, weeks or months, with exactly one unit, e.g. `{days: 3}`.
    #[serde(default)]
    #[schemars(with = "Every")]
    pub every: Raw,
    /// An iCalendar recurrence rule, e.g. `FREQ=MONTHLY;BYDAY=2TU`. Use `starting` for a rule with `INTERVAL` above 1 or `COUNT`, and `at` for its time of day.
    #[serde(default)]
    #[schemars(with = "String")]
    pub rrule: Raw,
    /// With `every` or `rrule`: the first date, `YYYY-MM-DD`. `every` needs it, and so does a rule with `INTERVAL` above 1, a `COUNT`, or a day it takes from its start.
    #[serde(default)]
    #[schemars(with = "String", regex(pattern = "^[0-9]{4}-[0-9]{2}-[0-9]{2}$"))]
    pub starting: Raw,
    /// With `every` or `rrule`: the time of day, `HH:MM` (24-hour), unless the rule sets `BYHOUR`/`BYMINUTE`. Default: 00:00.
    #[serde(default)]
    #[schemars(with = "String", regex(pattern = "^[0-9]{2}:[0-9]{2}$"))]
    pub at: Raw,
    /// Dates (`YYYY-MM-DD`, in its timezone) it doesn't fire on, e.g. holidays.
    #[serde(default)]
    #[schemars(with = "Vec<Date>")]
    pub except: Raw,
    /// Extra dates (`YYYY-MM-DD`) it fires on, at its time of day in its timezone. Needs a timing that fires at one time of day.
    #[serde(default)]
    #[schemars(with = "Vec<Date>")]
    pub also: Raw,
    #[serde(rename = "$unknown", default)]
    #[schemars(skip)]
    pub unknown: UnknownKeys,
}

impl ScheduleConfig {
    /// The keys it sets that a timing would set (its block and `timezone`), in the file's order.
    pub fn timing_keys(&self) -> Vec<&'static str> {
        let mut keys: Vec<(&'static str, (usize, usize))> = [
            ("cron", &self.cron),
            ("every", &self.every),
            ("rrule", &self.rrule),
            ("starting", &self.starting),
            ("at", &self.at),
            ("except", &self.except),
            ("also", &self.also),
        ]
        .into_iter()
        .filter_map(|(k, v)| v.as_ref().map(|v| (k, (v.line, v.column))))
        .collect();
        if let Some(tz) = &self.timezone {
            keys.push(("timezone", (tz.line, tz.column)));
        }
        keys.sort_by_key(|(_, at)| *at);
        keys.into_iter().map(|(k, _)| k).collect()
    }

    /// The schedule's own timing block, for [`crate::schedule::validate_block`].
    pub fn block(&self) -> JsonMap {
        block([
            ("cron", &self.cron),
            ("every", &self.every),
            ("rrule", &self.rrule),
            ("starting", &self.starting),
            ("at", &self.at),
            ("except", &self.except),
            ("also", &self.also),
        ])
    }
}

/// The keys that are written, in the file's order.
fn block<const N: usize>(keys: [(&str, &Raw); N]) -> JsonMap {
    let mut written: Vec<(&str, &Located<serde_json::Value>)> = keys
        .into_iter()
        .filter_map(|(k, v)| Some((k, v.as_ref()?)))
        .collect();
    written.sort_by_key(|(_, v)| (v.line, v.column));
    written
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.value.clone()))
        .collect()
}

/// Every N days, weeks or months, with exactly one unit.
#[derive(JsonSchema)]
#[schemars(deny_unknown_fields, inline, extend("minProperties" = 1, "maxProperties" = 1))]
#[allow(dead_code)]
struct Every {
    /// Every this many days.
    #[schemars(range(min = 1))]
    days: Option<u32>,
    /// Every this many weeks.
    #[schemars(range(min = 1))]
    weeks: Option<u32>,
    /// Every this many months.
    #[schemars(range(min = 1))]
    months: Option<u32>,
}

// A date, `YYYY-MM-DD` (no description of its own in the schema).
#[derive(JsonSchema)]
#[schemars(inline)]
#[allow(dead_code)]
struct Date(#[schemars(regex(pattern = "^[0-9]{4}-[0-9]{2}-[0-9]{2}$"))] String);

fn any_map(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({"type": "object", "additionalProperties": true})
}
