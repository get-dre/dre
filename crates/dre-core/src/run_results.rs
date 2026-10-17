//! `run_results.json`: what one run of one Binding did. A public, versioned contract
//! (`docs/run-results.schema.json` is generated from these types): adding an optional field
//! keeps [`SCHEMA_VERSION`]; removing, renaming or re-typing one, or changing what it means,
//! bumps it (a minor DRE release before 1.0, a major one after).

use std::collections::BTreeMap;
use std::path::PathBuf;

use schemars::JsonSchema;
use serde::Serialize;

use crate::codes::{Code, ErrorCode, Kind};
use crate::settings::Settings;

type JsonMap = serde_json::Map<String, serde_json::Value>;

/// The format's version.
pub const SCHEMA_VERSION: &str = "dre/run-results/v1";

/// `<target path>/run/<report>/<set or default>/runs/<run id>/run_results.json`: what one run of one Binding did. Written by `dre run` for each Binding it runs. See docs/manifest.md.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(title = "DRE run results")]
pub struct RunResults {
    /// The format's version: `dre/run-results/v1`.
    #[schemars(schema_with = "schema_version")]
    pub schema_version: &'static str,
    /// This run's id (`20261009T060000Z-k3f9`: its UTC start and four random characters), the name of its folder under `runs/`.
    pub run_id: String,
    pub report: String,
    /// Null for a report without Sets.
    pub set: Option<String>,
    /// The Binding's folder name: the Set's, or `default`.
    pub binding: String,
    /// False for a bare .sql under reports/ (an unmanaged report).
    pub managed: bool,
    /// The inherited connection (Set, report, folder `+profile`, `default_profile`), rendered.
    pub profile: Option<String>,
    /// Every connection the Binding's queries ran on.
    pub connections: Vec<String>,
    /// The run's target (environment): `--target`, else `DRE_TARGET`, else `dev`.
    pub target: String,
    /// The schedule it ran under (`--schedule`).
    pub schedule: Option<String>,
    /// The vars that schedule layered in.
    pub schedule_vars: Option<JsonMap>,
    /// Every var the Binding rendered with: its own, the schedule's, then `--var`.
    pub vars: JsonMap,
    /// `run.date`, `YYYY-MM-DD`.
    pub run_date: String,
    /// The instant it was scheduled for (`DRE_RUN_AT`), RFC 3339.
    pub scheduled_at: Option<String>,
    /// The run's timezone (IANA name).
    pub timezone: String,
    /// What the command was asked to do.
    pub params: Params,
    pub status: Status,
    /// Why it failed.
    pub error: Option<String>,
    /// `error`'s code (see the error codes reference).
    pub error_code: Option<ErrorCode>,
    /// `error_code`'s kind.
    pub error_kind: Option<Kind>,
    /// Whether it was a `--preview`: never delivered.
    pub preview: bool,
    /// `--preview`'s row limit per query.
    pub row_limit: Option<u64>,
    /// When the Binding started, RFC 3339.
    pub started_at: String,
    pub duration_ms: u64,
    /// Each result set its queries produced, in order.
    pub result_sets: Vec<ResultSet>,
    /// Every file every output wrote.
    pub outputs: Vec<OutputFile>,
    /// One entry per output, in declared order.
    pub output_results: Vec<OutputResult>,
    /// Why outputs weren't delivered, when that's by design (no destination, `deliver: false`, a preview).
    pub delivery: Option<String>,
    /// Every destination, in delivery order: file outputs, then messages.
    pub deliveries: Vec<Delivery>,
    /// How the result sets' columns changed since the last run, if they did.
    pub schema_drift: Vec<String>,
    /// The resolved target path.
    pub target_path: PathBuf,
    /// Every setting and where its value came from.
    #[schemars(schema_with = "settings")]
    pub settings: Settings,
    /// The SHA-256 of the manifest.json bytes this run wrote.
    pub manifest_checksum: Option<String>,
    /// `dre run --retry-failed <run id>`: the run whose failed and never-started rows this one redid.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_of: Option<String>,
    /// `dre release`: the run whose held deliveries this one delivered.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_of: Option<String>,
    /// `dre release --by` (default `$USER`): who released them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub released_by: Option<String>,
    /// Each held delivery a release delivered (or refused), with its result.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub released: Vec<Released>,
}

/// One held delivery, as `dre release` delivered it.
#[derive(Debug, Clone, Serialize, serde::Deserialize, JsonSchema)]
pub struct Released {
    /// The held entry's id in the run's `held.json`.
    pub id: String,
    pub output: Option<String>,
    /// A per-row output's row key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    pub profile: String,
    /// `delivered`, `failed`, `refused` (a file changed since it was rendered) or `not_delivered`.
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// How a Binding ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Success,
    Error,
    /// `--dry-run`: rendered, nothing executed.
    DryRun,
    /// `dre validate --live`: checked on the database, nothing executed.
    Checked,
    /// Stopped by Ctrl-C or a termination signal while it ran; nothing was delivered after.
    Cancelled,
    /// Stopped because the run took longer than its timeout; nothing was delivered after.
    TimedOut,
}

/// The command's parameters.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Params {
    pub selector: Option<String>,
    pub set: Option<String>,
    pub schedule: Option<String>,
    /// `--target` as given.
    pub target: Option<String>,
    /// `--profile`.
    pub profile: Option<String>,
    /// `--var`.
    pub vars: BTreeMap<String, String>,
    pub run_date: String,
    pub scheduled_at: Option<String>,
    /// `--timezone` or `DRE_TIMEZONE`.
    pub timezone: Option<String>,
    pub output_name: Option<String>,
    pub output_path: Option<String>,
    pub dry_run: bool,
    pub preview: Option<u64>,
    pub accept_schema_change: bool,
}

/// One result set.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ResultSet {
    /// Its name: the tab name.
    pub name: String,
    pub query: String,
    /// The connection it came from.
    pub connection: String,
    pub rows: u64,
    pub columns: Vec<String>,
}

/// One file an output wrote.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct OutputFile {
    /// Relative to the project root, or to the target path when that's outside the project.
    pub path: PathBuf,
    pub size: u64,
    /// Where it was delivered, when it was.
    pub delivered_to: Option<String>,
    /// The output's `name:`.
    pub output: Option<String>,
}

/// How one output ended.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct OutputResult {
    pub name: Option<String>,
    pub format: String,
    /// The queries it formatted.
    pub queries: Vec<String>,
    #[schemars(schema_with = "output_status")]
    pub status: &'static str,
    pub error: Option<String>,
    pub files: Vec<OutputFileRef>,
    pub delivery: Option<String>,
    pub deliveries: Vec<Delivery>,
    /// Its `when:`, evaluated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub when: Option<bool>,
    /// Why it was skipped or failed, when that's by design: `no_data` (`on_empty`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Its queries that returned no rows (a `no_data` warning when the output still went out).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub no_data: Vec<String>,
    /// Queries whose tab (or file) was left out because they were empty (`on_empty: skip`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub dropped: Vec<String>,
    /// A per-row output's summary; its rows are in the `sidecar`, not in `files` or `deliveries`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub for_each: Option<ForEachResult>,
    /// A message output's rendered title and text, as sent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<Message>,
}

/// A per-row output's summary: `output_results[].for_each`. Each row's detail is a line of the
/// `<output>.rows.jsonl` sidecar in the run folder.
#[derive(Debug, Clone, Default, Serialize, serde::Deserialize, JsonSchema)]
pub struct ForEachResult {
    /// The driver query.
    pub query: String,
    /// The key column(s).
    pub key: Vec<String>,
    /// Driver rows this run was asked to make (all, or the ones `--retry-failed`, `--only-keys` or `--limit-rows` chose).
    pub rows: u64,
    pub succeeded: u64,
    /// Rows with nothing to send (`on_empty`), or whose attachment failed.
    pub skipped: u64,
    pub failed: u64,
    /// Rows with deliveries held for `dre release`.
    pub held: u64,
    /// Rows that never started (the run was cancelled or timed out).
    #[serde(default)]
    pub not_started: u64,
    /// Rows of the output's queries whose key isn't in the driver.
    pub orphan_rows: u64,
    /// Each failed row: its key, error code and message (masked).
    pub failed_keys: Vec<FailedKey>,
    /// The sidecar with every row's result, relative to the project root (or the target path).
    pub sidecar: PathBuf,
    /// True when this run made only some rows (`--retry-failed`, `--only-keys`, `--limit-rows`).
    #[serde(default)]
    pub partial: bool,
}

/// A failed row.
#[derive(Debug, Clone, Serialize, serde::Deserialize, JsonSchema)]
pub struct FailedKey {
    pub key: String,
    pub code: String,
    pub message: String,
}

/// One line of `<output>.rows.jsonl`: one row of a per-row output.
#[derive(Debug, Clone, Serialize, serde::Deserialize, JsonSchema)]
pub struct RowRecord {
    pub key: String,
    /// `succeeded`, `skipped`, `failed` or `held`.
    pub status: String,
    /// Why a row was skipped: `no_data` or `attachment_failed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The files made for the row, with their SHA-256.
    #[serde(default)]
    pub files: Vec<RowFile>,
    /// Each destination of the row.
    #[serde(default)]
    pub deliveries: Vec<RowDelivery>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<FailedKey>,
}

/// One file of a row.
#[derive(Debug, Clone, Serialize, serde::Deserialize, JsonSchema)]
pub struct RowFile {
    pub path: PathBuf,
    pub sha256: String,
}

/// One delivery of a row. Values rendered from `each.*` are masked unless `mask_row_values: false`.
#[derive(Debug, Clone, Serialize, serde::Deserialize, JsonSchema)]
pub struct RowDelivery {
    pub profile: String,
    /// The rendered path, or the destination's address options (`to`, `channel`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// `delivered`, `failed`, `held`, `kept` (not delivered by design) or `not_delivered`.
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// One file of an output.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct OutputFileRef {
    pub path: PathBuf,
    pub size: u64,
    pub delivered_to: Option<String>,
}

/// A message output's rendered title and text.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Message {
    pub title: String,
    pub text: String,
}

/// One delivery to one destination.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Delivery {
    /// The destination profile.
    pub profile: String,
    /// Its plugin type; null for an entry that delivers nowhere.
    #[serde(rename = "type")]
    pub kind: Option<String>,
    /// The profile's entry for the run.
    pub target: String,
    #[schemars(schema_with = "delivery_status")]
    pub status: &'static str,
    /// Where it went.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    /// How many tries it took, when the plugin had to try again after a temporary error.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempts: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<ErrorCode>,
}

fn schema_version(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({"const": SCHEMA_VERSION})
}

fn output_status(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({"enum": ["delivered", "kept", "skipped", "failed", "held"]})
}

fn delivery_status(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({"enum": ["delivered", "not_delivered", "failed", "held"]})
}

fn settings(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": "array",
        "items": {
            "type": "object",
            "properties": {
                "name": {"type": "string"},
                "value": {"type": ["string", "null"]},
                "source": {"type": "string", "description": "A flag (`--target`), an environment variable (`DRE_TARGET`), a dre_project.yml key, how it was found, or `default`."}
            },
            "required": ["name", "value", "source"]
        },
        "x-dre-null": true
    })
}

impl JsonSchema for Code {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Code".into()
    }
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({"type": "string", "description": "A code from the error codes reference, e.g. `query-failed`."})
    }
    fn inline_schema() -> bool {
        true
    }
}

impl JsonSchema for ErrorCode {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "ErrorCode".into()
    }
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({"type": "string", "description": "A code from the error codes reference, e.g. `query-failed`, or a plugin's own, namespaced by the plugin, e.g. `sftp/host-key-mismatch`."})
    }
    fn inline_schema() -> bool {
        true
    }
}

impl JsonSchema for Kind {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Kind".into()
    }
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({"enum": ["usage", "config", "plugin", "refused", "connection", "auth", "query", "delivery", "internal", "cancelled", "timed_out"]})
    }
    fn inline_schema() -> bool {
        true
    }
}
