//! Delivery: send a set of files, or a message, to one destination entry.
//!
//! One [`Deliverer`] serves a Binding run, a per-row worker or a `dre release`. For each
//! [`Request`] it matches the entry in `profiles.yml` for the run's target, maps the entry's
//! `path` onto each file, applies the local store's rules (`if_exists`, `atomic`, `temp_dir`)
//! or hands the files to the destination plugin, using its message capabilities
//! (`message`, `message_only`, `multi_file`). Plugin processes are started on first use and
//! reused, one per destination type, until a delivery through one fails (it may be broken, so
//! the next request starts a fresh one) or the [`Deliverer`] is dropped.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use dre_protocol::host::{HostError, LogSink};
use dre_protocol::msg::DeliveryFile;
use dre_protocol::{CAP_MESSAGE, CAP_MESSAGE_ONLY, CAP_MULTI_FILE};
use serde_json::{Map as JsonMap, Value as Json};

use crate::codes::{Code, ErrorCode, Kind};
use crate::context::Context;
use crate::engine::Events;
use crate::error::Error as Fail;
use crate::plugin_host::Plugin;
use crate::profiles::{Entry, LOCAL_TYPE, Role};
use crate::project::{PluginKind, Project};
use crate::run::{Level, find_plugin, render_connection};

/// Checks a remote path before anything is sent, refusing it with an error.
pub type Claim<'r> = &'r dyn Fn(&str) -> Result<(), Fail>;

/// One delivery to one destination entry.
#[derive(Clone, Copy)]
pub struct Request<'r> {
    /// The destination profile, rendered.
    pub profile: &'r str,
    /// The entry's `path`, rendered: the remote path of a single file, or the folder (up to its
    /// last `/`) of several.
    pub path: Option<&'r str>,
    /// The entry's options, rendered.
    pub options: &'r JsonMap<String, Json>,
    /// The files to send: an output's files, or the one zip that packs them.
    pub files: &'r [PathBuf],
    /// A message output's title and text. A destination that takes messages gets the message;
    /// any other gets its `.md` file (the first of `files`).
    pub message: Option<&'r (String, String)>,
    /// Files attached to the message.
    pub attach: &'r [PathBuf],
    /// The output's format, for the error when a message-only destination gets files.
    pub format: Option<&'r str>,
    /// Called with each remote path before anything is sent, to refuse one (two Bindings
    /// running at once delivering to the same file).
    pub claim: Option<Claim<'r>>,
}

impl<'r> Request<'r> {
    /// Send `files` to destination `profile` at `path`, with `options`.
    pub fn new(
        profile: &'r str,
        path: Option<&'r str>,
        options: &'r JsonMap<String, Json>,
        files: &'r [PathBuf],
    ) -> Request<'r> {
        Request {
            profile,
            path,
            options,
            files,
            message: None,
            attach: &[],
            format: None,
            claim: None,
        }
    }

    /// Send a message (its `.md` file is `files[0]`), with `attach`ed files.
    #[must_use]
    pub fn message(mut self, message: Option<&'r (String, String)>, attach: &'r [PathBuf]) -> Request<'r> {
        self.message = message;
        self.attach = attach;
        self
    }

    /// The output's format.
    #[must_use]
    pub fn format(mut self, format: &'r str) -> Request<'r> {
        self.format = Some(format);
        self
    }

    /// Check each remote path with `claim` before sending.
    #[must_use]
    pub fn claim(mut self, claim: Claim<'r>) -> Request<'r> {
        self.claim = Some(claim);
        self
    }
}

/// What a delivery sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sent {
    /// Where it went: each send's location, joined by `, `.
    pub location: String,
    /// The most tries a send took (more than 1 when the plugin retried a temporary error).
    pub attempts: u32,
    /// Per file of the request, where it went (`None`: not sent on its own, as a message's
    /// `.md` beyond the first).
    pub files: Vec<Option<String>>,
}

/// Delivers to destination entries, reusing a plugin process per destination type.
pub struct Deliverer<'a> {
    ctx: Context,
    project: &'a Project,
    log: LogSink,
    /// Where `Kept` and `Delivered` steps go (a Binding run's; per-row and release log none).
    steps: Option<Events>,
    plugins: HashMap<String, Box<dyn Plugin>>,
}

impl<'a> Deliverer<'a> {
    /// Deliver for `project`, with `ctx`'s plugin host and environment; plugin log lines go to
    /// `log`.
    pub fn new(ctx: &Context, project: &'a Project, log: LogSink) -> Deliverer<'a> {
        Deliverer {
            ctx: ctx.clone(),
            project,
            log,
            steps: None,
            plugins: HashMap::new(),
        }
    }

    /// Report each delivery as a step on `events`.
    #[must_use]
    pub fn with_steps(mut self, events: Events) -> Deliverer<'a> {
        self.steps = Some(events);
        self
    }

    /// Deliver `r`. `Ok(None)`: the entry delivers nowhere for this target (`deliver: false`).
    pub fn deliver(&mut self, r: Request<'_>) -> Result<Option<Sent>, Fail> {
        self.send(r).map_err(|e| e.or(Code::DeliveryFailed))
    }

    fn send(&mut self, r: Request<'_>) -> Result<Option<Sent>, Fail> {
        let profiles = &self.project.profiles;
        let entry = match profiles.entry(Role::Destination, r.profile) {
            Entry::Use(o) => o,
            Entry::Nowhere => {
                if let Some(steps) = &self.steps {
                    let target = profiles.target_of(Role::Destination, r.profile);
                    steps.step(Level::Info, "Kept", &nowhere_note(r.profile, &target), None);
                }
                return Ok(None);
            }
            // Both are checked before a run starts.
            Entry::Missing => {
                return Err(Fail::new(
                    Code::MissingTargetEntry,
                    profiles.missing_entry(Role::Destination, r.profile),
                ));
            }
            Entry::Unknown => {
                return Err(format!("destination profile `{}` isn't in profiles.yml", r.profile).into());
            }
        };
        let kind = entry.kind.clone();
        let connection = render_connection(entry, self.ctx.env())?;
        let targets = targets(r.path, r.files);
        if let Some(claim) = r.claim {
            for remote in targets.iter().filter_map(|t| t.remote_path.as_deref()) {
                claim(remote)?;
            }
        }
        if kind == LOCAL_TYPE {
            return self.send_local(&r, &targets).map(Some);
        }
        let steps = self.steps.clone();
        let p = self.plugin(&kind)?;
        let result = send_through(p, &kind, &r, &targets, connection, steps.as_ref());
        if result.is_err() {
            // A destination plugin that failed may be broken: the next delivery starts a fresh one.
            if let Some(p) = self.plugins.remove(&kind) {
                p.close();
            }
        }
        result.map(Some)
    }

    /// Copy the files into the project, by the local store's rules.
    fn send_local(&self, r: &Request<'_>, targets: &[DeliveryFile]) -> Result<Sent, Fail> {
        let errors = crate::options::local_option_errors(r.options);
        if !errors.is_empty() {
            return Err(format!("destination `{}`: {}", r.profile, errors.join("; ")).into());
        }
        // `if_exists`, `atomic` (default true: written as `.<name>.dre-part`, then renamed) and
        // `temp_dir`.
        let (rules, _) = dre_protocol::delivery::Rules::from_settings(
            dre_protocol::delivery::Rules {
                retries: 0,
                ..Default::default()
            },
            &JsonMap::new(),
            r.options,
            &[],
        )?;
        let mut files = Vec::new();
        for f in targets {
            let t = Instant::now();
            let remote = f
                .remote_path
                .as_ref()
                .ok_or("the local destination needs `output.destination.path`")?;
            let dst = self.project.root.join(remote).to_string_lossy().to_string();
            let delivered = dre_protocol::delivery::deliver(
                &mut dre_protocol::delivery::LocalStore,
                Path::new(&f.local_path),
                &dst,
                &rules,
            )
            .map_err(|e| {
                let message = format!("delivery to {dst} failed: {e}; the output is still in target/");
                if e.exists {
                    // `if_exists: error`, coded as a plugin's would be.
                    let code = ErrorCode::Plugin {
                        code: "local/file-exists".into(),
                        kind: Kind::Delivery,
                    };
                    Fail::new(code, message)
                } else {
                    Fail::new(Code::DeliveryFailed, message)
                }
            })?;
            if let Some(steps) = &self.steps {
                steps.step(Level::Debug, "Delivered", &delivered.path, Some(t.elapsed()));
            }
            files.push(Some(delivered.path));
        }
        Ok(Sent {
            location: files.iter().flatten().cloned().collect::<Vec<_>>().join(", "),
            attempts: 1,
            files,
        })
    }

    /// The running plugin for destination type `kind`, started on first use.
    fn plugin(&mut self, kind: &str) -> Result<&mut dyn Plugin, Fail> {
        if !self.plugins.contains_key(kind) {
            let located = find_plugin(&self.ctx, self.project, PluginKind::Destination, kind)
                .map_err(|e| Fail::new(Code::DeliveryFailed, e.to_string()))?;
            let p = self
                .ctx
                .host()
                .start(&located, self.log.clone(), Some(&self.project.root))
                .map_err(|e| Fail::new(Code::DeliveryFailed, e.to_string()))?;
            self.plugins.insert(kind.to_string(), p);
        }
        Ok(self.plugins.get_mut(kind).expect("started above").as_mut())
    }
}

impl Drop for Deliverer<'_> {
    fn drop(&mut self) {
        for (_, p) in self.plugins.drain() {
            p.close();
        }
    }
}

/// Hand the files (or the message) to a destination plugin.
fn send_through(
    p: &mut dyn Plugin,
    kind: &str,
    r: &Request<'_>,
    targets: &[DeliveryFile],
    connection: JsonMap<String, Json>,
    steps: Option<&Events>,
) -> Result<Sent, Fail> {
    let failed = |e: HostError| {
        Fail::from_plugin(
            &e,
            format!("delivery through `{kind}` failed: {e}; the output is still in target/"),
        )
    };
    let step = |location: &str, t: Instant| {
        if let Some(steps) = steps {
            steps.step(Level::Debug, "Delivered", location, Some(t.elapsed()));
        }
    };
    if let Some((title, text)) = r.message
        && p.has(CAP_MESSAGE)
    {
        let t = Instant::now();
        let path = targets
            .first()
            .map(|f| f.local_path.clone())
            .ok_or("the message has no file to deliver")?;
        let message = dre_protocol::msg::Message {
            title: title.clone(),
            text: text.clone(),
            html: None,
            path,
        };
        let attach = r
            .attach
            .iter()
            .map(|f| DeliveryFile {
                local_path: f.to_string_lossy().to_string(),
                remote_path: None,
            })
            .collect();
        let delivered = p
            .deliver(crate::plugin_host::Delivery {
                files: attach,
                message: Some(message),
                connection,
                options: r.options.clone(),
            })
            .map_err(failed)?;
        step(&delivered.location, t);
        let mut files = vec![None; targets.len()];
        if let Some(first) = files.first_mut() {
            *first = Some(delivered.location.clone());
        }
        return Ok(Sent {
            location: delivered.location,
            attempts: delivered.attempts,
            files,
        });
    }
    if r.message.is_none() && p.has(CAP_MESSAGE_ONLY) {
        let what = match r.format {
            Some(f) => format!("this output is `{f}`"),
            None => "these are files".to_string(),
        };
        return Err(format!(
            "`{kind}` only takes messages, but {what}; deliver the file elsewhere and link it from a message"
        )
        .into());
    }
    let batches: Vec<Vec<usize>> = if targets.len() > 1 && p.has(CAP_MULTI_FILE) {
        vec![(0..targets.len()).collect()]
    } else {
        (0..targets.len()).map(|i| vec![i]).collect()
    };
    let mut sent = Sent {
        location: String::new(),
        attempts: 1,
        files: vec![None; targets.len()],
    };
    let mut locations = Vec::new();
    for batch in batches {
        let t = Instant::now();
        let delivered = p
            .deliver(crate::plugin_host::Delivery {
                files: batch.iter().map(|&i| targets[i].clone()).collect(),
                message: None,
                connection: connection.clone(),
                options: r.options.clone(),
            })
            .map_err(failed)?;
        sent.attempts = sent.attempts.max(delivered.attempts);
        step(&delivered.location, t);
        for i in batch {
            sent.files[i] = Some(delivered.location.clone());
        }
        locations.push(delivered.location);
    }
    sent.location = locations.join(", ");
    Ok(sent)
}

/// Each file with its remote path: the entry's `path` for a single file; for several, each
/// file's name in the folder `path` names (up to its last `/`).
pub fn targets(path: Option<&str>, files: &[PathBuf]) -> Vec<DeliveryFile> {
    files
        .iter()
        .map(|f| {
            let name = f.file_name().unwrap_or_default().to_string_lossy();
            let remote = path.map(|p| {
                if files.len() == 1 {
                    p.to_string()
                } else {
                    match p.rfind(['/', '\\']) {
                        Some(i) => format!("{}{name}", &p[..=i]),
                        None => name.to_string(),
                    }
                }
            });
            DeliveryFile {
                local_path: f.to_string_lossy().to_string(),
                remote_path: remote,
            }
        })
        .collect()
}

/// What a run logs, and records, for a destination whose entry is `deliver: false`.
pub fn nowhere_note(profile: &str, target: &str) -> String {
    format!(
        "destination `{profile}`: `{target}` delivers nowhere (`deliver: false`); output stays in target/"
    )
}

#[cfg(test)]
mod tests;
