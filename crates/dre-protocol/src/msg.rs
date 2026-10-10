//! Control messages. Every JSON frame is an object with a `type` field. From protocol 1, every
//! message after the handshake also carries the `id` of the request it belongs to ([`Envelope`]).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub use crate::options::OptionField;
use crate::{Kind, PluginId};

/// A message with the id of the request it belongs to. Core numbers its requests; every reply,
/// and every message streamed as part of a request (`result_set_end`, `finish`, `result`,
/// `result_end`, `log`, `progress`), carries that request's id. `hello` has none, and neither
/// does anything a protocol 0 peer sends.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope<T> {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,
    #[serde(flatten)]
    pub body: T,
}

impl<T> Envelope<T> {
    pub fn new(id: Option<u64>, body: T) -> Envelope<T> {
        Envelope { id, body }
    }
}

/// Core → plugin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    /// Always first. The plugin picks a version in both ranges or replies `version_mismatch`.
    Hello {
        min_version: u32,
        max_version: u32,
        core_version: String,
        /// The plugin core wants served, for an executable that provides several. A package
        /// that doesn't provide it replies `error`; without it, the package serves its first.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        plugin: Option<PluginId>,
    },
    /// Describe the connection fields a profile output for this plugin takes (used by `dre init`)
    /// and the options a report's config block for it takes.
    Describe {},
    /// Check one config block of options (a format's `output:` keys, a destination entry's keys)
    /// without doing anything. Replies `validated`.
    Validate {
        options: Map<String, Value>,
    },
    /// Source: open a session. All later `execute`/`check` requests run on it.
    Open {
        connection: Map<String, Value>,
        read_only: bool,
    },
    /// Source: run one statement. Replies `result` (then Arrow frames, then `result_end`) or
    /// `no_result`.
    Execute {
        sql: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        row_limit: Option<u64>,
    },
    /// Source: verify one statement without executing it. Replies `ok` or `error`.
    Check {
        sql: String,
    },
    /// Format: write result sets to `path`. Followed, per result set, by Arrow frames (at least one,
    /// carrying the schema) and a `result_set_end`, then `finish`. Replies `written`.
    Write {
        path: String,
        format: String,
        options: Map<String, Value>,
        result_sets: Vec<ResultSetMeta>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        template: Option<Value>,
    },
    ResultSetEnd {},
    Finish {},
    /// Destination: deliver a local file, or with `files` (only to a plugin advertising
    /// `multi_file`) every file of one output at once. Exactly one of `local_path`/`files` is set,
    /// unless `message` is: then `files` (possibly empty) holds the files to attach to it, and is
    /// sent only to a plugin advertising `message`. `options` are the destination entry's plugin
    /// options, rendered by core. Replies `delivered`.
    Deliver {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        local_path: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        remote_path: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        files: Vec<DeliveryFile>,
        connection: Map<String, Value>,
        #[serde(default)]
        options: Map<String, Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<Message>,
    },
    /// Source: load rows into a temporary table on the session, named after `name`. Followed by
    /// Arrow frames (at least one, carrying the schema) and a `result_set_end`. Replies `loaded`.
    /// Only sent to plugins advertising `load`.
    Load {
        name: String,
    },
    /// End the conversation; the plugin exits 0.
    Close {},
    /// Protocol 1: stop the request whose id the envelope carries. It has no reply of its own:
    /// the request replies `error` with kind `cancelled`, or its usual reply if it finished
    /// first. A cancel for a request that isn't running is ignored.
    Cancel {},
}

/// Plugin → core.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Hello {
        protocol_version: u32,
        kind: Kind,
        name: String,
        version: String,
        #[serde(default)]
        capabilities: Vec<String>,
        /// Every plugin the executable serves; empty for one that serves only `kind`/`name`.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        provides: Vec<PluginId>,
    },
    VersionMismatch {
        min_version: u32,
        max_version: u32,
    },
    Describe {
        connection_fields: Vec<ConnectionField>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        option_fields: Vec<OptionField>,
        /// Source: the character the database quotes identifiers with (`"` for DuckDB and
        /// Postgres, a backtick for Databricks). Core uses it to quote names a project asks to
        /// quote (a source's `quoting:`). Every source sets it; other kinds leave it out.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        identifier_quote: Option<String>,
        /// Destination advertising `message`: the most characters a message may have in the
        /// service, after translation. Core shows it next to a message's length in a preview.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message_limit: Option<u64>,
    },
    /// Every problem with the options, each a sentence naming the key; empty when they're fine.
    Validated {
        errors: Vec<String>,
    },
    Ok {},
    /// A result set follows as Arrow frames, ended by `result_end`.
    Result {
        columns: Vec<String>,
    },
    ResultEnd {
        rows: u64,
    },
    NoResult {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rows_affected: Option<u64>,
    },
    Written {
        files: Vec<String>,
        /// Things the person should know about the files (a value written as text, say).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        warnings: Vec<String>,
    },
    Delivered {
        location: String,
    },
    /// `relation` is what SQL uses to read the loaded rows. `warning`, when set, is shown to the
    /// user (e.g. the database has no bulk path, so a load this size is slow).
    Loaded {
        relation: String,
        rows: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        warning: Option<String>,
    },
    /// The request failed. `kind` is one of [`ERROR_KINDS`]; `code` names the problem,
    /// namespaced by the plugin (`sftp/host-key-mismatch`). Both are optional (protocol 0 has
    /// neither): without a `code`, core reports the failure under its own code for the step;
    /// a `code` without a `kind` counts as `internal`.
    Error {
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<String>,
    },
    /// Protocol 1: a log line, while a request runs (with its id) or between requests.
    Log {
        level: LogLevel,
        message: String,
        /// Structured context (`{"attempt": 2}`); never secrets.
        #[serde(default, skip_serializing_if = "Map::is_empty")]
        fields: Map<String, Value>,
    },
    /// Protocol 1, optional: how far a long request has got. Core shows it when present.
    Progress {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        done: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        total: Option<u64>,
    },
}

impl Response {
    /// An `error` reply with only a message.
    pub fn error(message: impl Into<String>) -> Response {
        Response::Error {
            message: message.into(),
            kind: None,
            code: None,
        }
    }
}

/// The kinds an `error` reply may give, from DRE's error-code registry.
pub const ERROR_KINDS: &[&str] = &[
    "config",
    "plugin",
    "refused",
    "connection",
    "auth",
    "query",
    "delivery",
    "internal",
    "cancelled",
    "timed_out",
];

/// How important a `log` message is. Core shows `info` and above to the person, and keeps
/// `debug` and `trace` for `-v` and the log file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

/// A rendered message, as `deliver` carries it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    /// One line, plain text.
    pub title: String,
    /// The portable Markdown subset ([`crate::markdown`]).
    pub text: String,
    /// An HTML body, when one was made; core leaves it out today. A plugin that can show HTML
    /// (email) uses it when present, else converts `text`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub html: Option<String>,
    /// The `.md` file core wrote: `# title`, then the text.
    pub path: String,
}

/// One file of a multi-file `deliver`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeliveryFile {
    pub local_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_path: Option<String>,
}

/// Per result set, what a format plugin needs to lay it out.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResultSetMeta {
    /// Sheet name (xlsx) or file suffix (single-table formats).
    pub name: String,
    /// The query (`.sql` basename) that produced it.
    pub query: String,
    /// 1-based index among that query's result sets.
    pub result_index: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<bool>,
    /// The query entry's `columns:` map: per result column, how to show it.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub columns: std::collections::BTreeMap<String, ColumnOptions>,
}

/// One entry of a `columns:` map.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ColumnOptions {
    /// An Excel number format code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    /// A row formula (`={qty}*{price}`), written in place of the column's value, which becomes
    /// its cached result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub formula: Option<String>,
    /// A totals row entry under the data: `sum`, `average`, `count`, `min`, `max`, or a formula
    /// over whole columns (`=SUM({amount:*})/COUNT({qty:*})`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConnectionField {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub required: bool,
    /// Offered as an `env_var()` reference by default in `dre init`.
    #[serde(default)]
    pub secret: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    /// A source type whose profile has the same field: `dre init` offers the value entered for
    /// that source as this field's default (e.g. one Databricks host for source and destination).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub same_as_source: Option<String>,
    /// Set by hand in `profiles.yml`: `dre init` doesn't ask for it. For alternatives to a
    /// prompted field (a key given as text instead of a file) and nested blocks (`ssh:`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub manual: bool,
}

impl ConnectionField {
    pub fn new(name: &str, description: &str) -> Self {
        ConnectionField {
            name: name.into(),
            description: description.into(),
            required: false,
            secret: false,
            default: None,
            same_as_source: None,
            manual: false,
        }
    }
    pub fn required(mut self) -> Self {
        self.required = true;
        self
    }
    pub fn secret(mut self) -> Self {
        self.secret = true;
        self
    }
    pub fn default(mut self, v: impl Into<Value>) -> Self {
        self.default = Some(v.into());
        self
    }
    pub fn same_as_source(mut self, source_type: &str) -> Self {
        self.same_as_source = Some(source_type.into());
        self
    }
    pub fn manual(mut self) -> Self {
        self.manual = true;
        self
    }
}
