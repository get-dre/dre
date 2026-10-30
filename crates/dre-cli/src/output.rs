//! Terminal output: right-aligned coloured status verbs, a live progress bar, log levels,
//! JSON lines for machines, and for `dre run` a full debug log per run, `dre.log` in each
//! Binding's run folder: every event, and the full text of every SQL statement sent to a
//! database. It goes with its run when runs are pruned, so it never needs rotating; overlapping
//! runs each write their own. Other commands write to the console only.
//!
//! Lines go to stdout. The progress bars (Bindings, and each per-row output's rows) go to stderr
//! and only appear when stderr is a terminal; without one, row progress is a `Progress` line at
//! most every 30 seconds. Colour follows `--color`, `NO_COLOR` and whether stdout is a terminal.

use std::fs::File;
use std::io::{IsTerminal, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anstyle::{AnsiColor, Effects, Style};
use dre_core::engine::RunEvent;
use dre_core::run::{BindingOutcome, Level, Status};
use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};

use crate::progress::{RowMeter, describe, short_name};
use dre_core::secrets::Secrets;
use serde_json::json;

// Protect strings and keys while they still hold literal values, including JSON fragments
// embedded in ordinary log lines. Text masking cannot recover values after JSON escaping.
fn protected_json<T: serde::Serialize>(secrets: &Secrets, value: &T) -> String {
    secrets.to_json_line(value).expect("JSON output must serialize")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, clap::ValueEnum)]
pub enum Verbosity {
    /// Errors and the final summary only.
    Quiet,
    /// One line per Binding, plus warnings (the default).
    Info,
    /// Every step: rendering, each statement, formatting, delivery, plugin logs.
    Debug,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum LogFormat {
    Text,
    /// One JSON object per line, for CI and tooling.
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ColorChoice {
    Auto,
    Always,
    Never,
}

#[derive(Clone, Copy)]
pub enum Tone {
    Good,
    Bad,
    Warn,
    Step,
    Note,
}

fn style(t: Tone) -> Style {
    let c = match t {
        Tone::Good => AnsiColor::Green,
        Tone::Bad => AnsiColor::Red,
        Tone::Warn => AnsiColor::Yellow,
        Tone::Step => AnsiColor::Cyan,
        Tone::Note => AnsiColor::Blue,
    };
    Style::new().fg_color(Some(c.into())).effects(Effects::BOLD)
}

const DIM: Style = Style::new().effects(Effects::DIMMED);

/// One per-row output's progress: its meter, and its bar on a terminal.
struct Rows {
    meter: RowMeter,
    bar: Option<ProgressBar>,
}

struct Inner {
    /// What every line masks: the command's secrets.
    secrets: Secrets,
    verbosity: Verbosity,
    format: LogFormat,
    color: bool,
    bar: Option<ProgressBar>,
    /// Draws the Bindings bar and the row bars together (a terminal only).
    multi: Option<MultiProgress>,
    /// Each per-row output in progress, by its label.
    rows: std::collections::HashMap<String, Rows>,
    log: Option<LogFile>,
    /// `dre run`: each Binding logs to `dre.log` in its run folder.
    run_logs: bool,
    /// Lines logged before the first Binding (the run's settings): they open every Binding's log.
    header: Vec<String>,
    /// Lines logged since the Binding started, before its log file was known.
    pending: Vec<String>,
    in_binding: bool,
    seen_binding: bool,
    current: String,
    /// Bindings running at once (`threads:`), shown on the progress bar.
    running: Vec<String>,
    started: Instant,
    succeeded: usize,
    failed: usize,
}

/// Renders run events. Cheap to clone; clones share state (plugin log lines arrive on threads).
#[derive(Clone)]
pub struct Printer {
    inner: Arc<Mutex<Inner>>,
}

impl Printer {
    pub fn new(verbosity: Verbosity, format: LogFormat, color: ColorChoice, secrets: Secrets) -> Printer {
        let color = match color {
            ColorChoice::Always => true,
            ColorChoice::Never => false,
            ColorChoice::Auto => {
                std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty()) && std::io::stdout().is_terminal()
            }
        } && format == LogFormat::Text;
        Printer {
            inner: Arc::new(Mutex::new(Inner {
                secrets,
                verbosity,
                format,
                color,
                bar: None,
                multi: None,
                rows: Default::default(),
                log: None,
                run_logs: false,
                header: Vec::new(),
                pending: Vec::new(),
                in_binding: false,
                seen_binding: false,
                current: String::new(),
                running: Vec::new(),
                started: Instant::now(),
                succeeded: 0,
                failed: 0,
            })),
        }
    }

    /// Run `f` with the shared state locked. `f` must not call back into the `Printer`: the
    /// lock isn't reentrant.
    fn with<R>(&self, f: impl FnOnce(&mut Inner) -> R) -> R {
        f(&mut self.inner.lock().unwrap())
    }

    /// `dre run`: also write every event, at debug level, to `dre.log` in each Binding's run
    /// folder (see the module docs).
    pub fn log_runs(&self) {
        self.with(|i| i.run_logs = true);
    }

    /// Print a diagnostic: a line of text, or with `--log-format json` a `diagnostic` event, so
    /// warnings never break the JSON stream.
    pub fn diag(&self, d: &dre_core::Diagnostic) {
        // Text is printed after the lock is released, as before.
        let text = self.with(|i| {
            if i.format != LogFormat::Json {
                return Some(i.diagnostic(d));
            }
            let message = i.secrets.mask(&d.message).into_owned();
            i.json(
                json!({"event": "diagnostic", "severity": d.severity, "code": d.code,
                "file": d.file, "line": d.line, "message": message}),
            );
            (d.code == dre_core::codes::Code::OtherReportErrors).then(|| i.diagnostic(d))
        });
        if let Some(text) = text {
            if d.code == dre_core::codes::Code::OtherReportErrors {
                eprintln!("{text}");
            } else {
                println!("{text}");
            }
        }
    }

    /// `     Verb  text`, the verb right-aligned in a 10-column gutter.
    pub fn line(&self, tone: Tone, verb: &str, text: &str) {
        self.with(|i| i.line(tone, verb, text, Level::Info));
    }

    /// Like [`Printer::line`], shown only with `-v`, and only as text: JSON events carry the
    /// same facts in their own fields.
    pub fn detail(&self, tone: Tone, verb: &str, text: &str) {
        self.with(|i| {
            if i.format == LogFormat::Json {
                i.file_log("DEBUG", &format!("{verb} {text}"));
                return;
            }
            i.line(tone, verb, text, Level::Debug);
        })
    }

    pub fn error(&self, msg: &str) {
        self.with(|i| {
            i.file_log("ERROR", msg);
            if i.format == LogFormat::Json {
                i.json(json!({"event": "error", "message": msg}));
            } else {
                i.print(Tone::Bad, "Error", msg);
            }
        })
    }

    /// Record the run's parameters in the log file (and as a JSON event).
    pub fn log_params(&self, params: &serde_json::Value) {
        self.with(|i| {
            i.file_log(
                "INFO",
                &format!("Parameters {}", protected_json(&i.secrets, params)),
            );
            if i.format == LogFormat::Json {
                i.json(json!({"event": "run_parameters", "parameters": params}));
            }
        })
    }

    /// Where a compiled Binding's output would go: source, output file, each destination and
    /// its target (non-dev targets stand out), and the schedules that run it.
    pub fn plan(&self, p: &dre_core::run::BindingPlan) {
        self.with(|i| {
            let non_dev = |t: &str| t != "dev";
            i.print(Tone::Step, "Binding", &label(&p.report, p.set.as_deref()));
            for f in &p.compiled {
                i.print(Tone::Good, "Compiled", &f.display().to_string());
            }
            i.print(
                if non_dev(&p.target) {
                    Tone::Warn
                } else {
                    Tone::Note
                },
                "Target",
                &p.target,
            );
            for q in &p.queries {
                let sources = if q.sources.is_empty() {
                    String::new()
                } else {
                    format!(", reads {}", q.sources.join(", "))
                };
                let target = if q.target == p.target {
                    String::new()
                } else {
                    format!(", target {}", q.target)
                };
                i.print(
                    Tone::Note,
                    "Query",
                    &format!("{} on {} ({}){target}{sources}", q.query, q.connection, q.kind),
                );
            }
            for o in &p.outputs {
                let what = match &o.name {
                    Some(n) => format!("{} ({}, output `{n}`)", o.output.display(), o.format),
                    None => format!("{} ({})", o.output.display(), o.format),
                };
                i.print(Tone::Note, "Output", &what);
                for d in &o.destinations {
                    let target = d.target.clone().unwrap_or_default();
                    let what = match (&d.kind, &d.path) {
                        (Some(k), Some(path)) => format!("{} ({k}), target {target} → {path}", d.profile),
                        (Some(k), None) => format!("{} ({k}), target {target}", d.profile),
                        (None, _) => format!("{}: `{target}` delivers nowhere (`deliver: false`)", d.profile),
                    };
                    let tone = if d.delivers && non_dev(&target) {
                        Tone::Warn
                    } else {
                        Tone::Note
                    };
                    i.print(tone, if d.delivers { "Delivers" } else { "Keeps" }, &what);
                }
            }
            if !p.schedules.is_empty() {
                i.print(Tone::Note, "Schedules", &p.schedules.join(", "));
            }
        })
    }

    /// Final line of a run.
    pub fn finish(&self, what: &str) {
        self.with(|i| {
            if let Some(b) = i.bar.take() {
                b.finish_and_clear();
            }
            for (_, r) in i.rows.drain() {
                if let Some(b) = r.bar {
                    b.finish_and_clear();
                }
            }
            let (ok, failed) = (i.succeeded, i.failed);
            let secs = i.started.elapsed().as_secs_f64();
            let text = format!("'{what}' in {} · {ok} succeeded, {failed} failed", fmt_secs(secs));
            i.file_log("INFO", &format!("Finished {text}"));
            if i.format == LogFormat::Json {
                i.json(json!({"event": "finished", "command": what, "succeeded": ok, "failed": failed, "elapsed_ms": (secs * 1000.0) as u64}));
            } else {
                i.print(if failed > 0 { Tone::Bad } else { Tone::Good }, "Finished", &text);
            }
        })
    }
}

impl Inner {
    /// A diagnostic's text, its `error[...]`/`warning[...]` prefix coloured.
    fn diagnostic(&self, d: &dre_core::Diagnostic) -> String {
        let text = self.secrets.mask(&d.to_string()).into_owned();
        let tone = match d.severity {
            dre_core::Severity::Error => Tone::Bad,
            dre_core::Severity::Warning => Tone::Warn,
        };
        match text.find("]: ") {
            Some(end) if self.color => {
                format!(
                    "{}{}",
                    self.paint(style(tone), &text[..end + 1]),
                    &text[end + 1..]
                )
            }
            _ => text,
        }
    }

    fn paint(&self, s: Style, text: &str) -> String {
        if self.color {
            format!("{s}{text}{s:#}")
        } else {
            text.to_string()
        }
    }

    fn print(&self, tone: Tone, verb: &str, text: &str) {
        let verb = self.paint(style(tone), &format!("{verb:>10}"));
        // Continuation lines line up under the text; blank ones stay blank.
        let text = text
            .lines()
            .enumerate()
            .map(|(n, l)| {
                if n == 0 || l.trim().is_empty() {
                    l.trim_end().to_string()
                } else {
                    format!("{:>10}  {l}", "")
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let out = format!("{verb}  {text}");
        let out = self.secrets.mask(&out).into_owned();
        self.suspend(|| print_line(&out));
    }

    /// Run `f` (which prints) with the progress bars out of the way.
    fn suspend<R>(&self, f: impl FnOnce() -> R) -> R {
        match (&self.multi, &self.bar) {
            (Some(m), _) => m.suspend(f),
            (None, Some(b)) => b.suspend(f),
            (None, None) => f(),
        }
    }

    /// The bars' shared drawing area, made on first use.
    fn multi(&mut self) -> MultiProgress {
        self.multi
            .get_or_insert_with(|| MultiProgress::with_draw_target(ProgressDrawTarget::stderr()))
            .clone()
    }

    fn rows(&mut self, p: &dre_core::run::RowProgress) {
        if self.verbosity == Verbosity::Quiet {
            return;
        }
        let now = Instant::now();
        if p.finished {
            if let Some(Rows { bar: Some(b), .. }) = self.rows.remove(&p.output) {
                b.finish_and_clear();
            }
            return;
        }
        let tty = self.format == LogFormat::Text && std::io::stderr().is_terminal();
        if tty && !self.rows.get(&p.output).is_some_and(|r| r.bar.is_some()) {
            let bar = self.multi().add(ProgressBar::new(p.total));
            bar.set_style(
                ProgressStyle::with_template("{prefix:>10.cyan.bold} [{bar:28.green/dim}] {wide_msg}")
                    .expect("the progress template is valid")
                    .progress_chars("█▉░"),
            );
            bar.set_prefix(short_name(&p.output).to_string());
            bar.enable_steady_tick(Duration::from_millis(250));
            self.rows.insert(
                p.output.clone(),
                Rows {
                    meter: RowMeter::new(now),
                    bar: Some(bar),
                },
            );
        }
        let r = self.rows.entry(p.output.clone()).or_insert_with(|| Rows {
            meter: RowMeter::new(now),
            bar: None,
        });
        r.meter.record(now, p.done);
        let speed = r.meter.speed(now);
        if let Some(b) = &r.bar {
            b.set_length(p.total);
            b.set_position(p.done);
            b.set_message(describe(p, speed, " · "));
            return;
        }
        if !r.meter.line_due(now) {
            return;
        }
        if self.format == LogFormat::Json {
            let left = speed
                .filter(|s| *s > 0.0)
                .map(|s| ((p.total - p.done) as f64 / s).ceil() as u64);
            self.json(json!({
                "event": "rows_progress", "output": p.output, "done": p.done, "total": p.total,
                "failed": p.failed, "rate": p.rate.map(|r| r.shown()), "rows_per_second": speed,
                "seconds_left": left,
            }));
        } else {
            let text = format!("{}: {}", p.output, describe(p, speed, ", "));
            self.line(Tone::Step, "Progress", &text, Level::Info);
        }
    }

    fn line(&mut self, tone: Tone, verb: &str, text: &str, level: Level) {
        self.file_log(
            if level == Level::Debug { "DEBUG" } else { "INFO" },
            &format!("{verb} {text}"),
        );
        if !self.shows(level) {
            return;
        }
        if self.format == LogFormat::Json {
            self.json(json!({"event": "line", "verb": verb, "message": text}));
        } else {
            self.print(tone, verb, text);
        }
    }

    fn shows(&self, level: Level) -> bool {
        match self.verbosity {
            Verbosity::Quiet => false,
            Verbosity::Info => level == Level::Info,
            Verbosity::Debug => true,
        }
    }

    fn json(&self, mut v: serde_json::Value) {
        v["ts"] = json!(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
        let line = self
            .secrets
            .to_json_line(&v)
            .expect("serializing a serde_json::Value cannot fail");
        self.suspend(|| print_line(&line));
    }

    fn file_log(&mut self, level: &str, msg: &str) {
        if !self.run_logs {
            return;
        }
        let ts = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let ctx = if self.current.is_empty() {
            String::new()
        } else {
            format!(" [{}]", self.current)
        };
        let line = self
            .secrets
            .mask(&format!("{ts} {level:<5}{ctx} {msg}\n"))
            .into_owned();
        match self.log.as_mut() {
            Some(f) => f.write(&line),
            None if !self.seen_binding => self.header.push(line),
            None if self.in_binding => self.pending.push(line),
            None => {}
        }
    }
}

/// A run's log file, appended to.
struct LogFile {
    file: File,
}

impl LogFile {
    fn open(path: &Path) -> Option<LogFile> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok()?;
        Some(LogFile { file })
    }

    fn write(&mut self, entry: &str) {
        let _ = self.file.write_all(entry.as_bytes());
    }
}

/// Print a line as is: JSON events are already redacted, and masking the text again could break them.
fn print_line(s: &str) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{s}");
}

fn fmt_secs(s: f64) -> String {
    if s < 60.0 {
        format!("{s:.2}s")
    } else {
        format!("{}m {:02}s", (s / 60.0) as u64, (s % 60.0) as u64)
    }
}

/// `text`'s first line, at most `max` characters, ending in `…` when cut.
fn one_line(text: &str, max: usize) -> String {
    let line = text.lines().next().unwrap_or("");
    if line.chars().count() <= max {
        line.to_string()
    } else {
        let mut cut: String = line.chars().take(max.saturating_sub(1)).collect();
        cut.push('…');
        cut
    }
}

fn timing(d: Duration) -> String {
    format!("[{:>6}]", fmt_secs(d.as_secs_f64()))
}

fn label(report: &str, set: Option<&str>) -> String {
    match set {
        Some(s) => format!("{report} [{s}]"),
        None => report.to_string(),
    }
}

impl dre_core::start::Console for Printer {
    fn installed(&self, what: &str) {
        self.line(Tone::Note, "Installed", what);
    }
    fn error(&self, message: &str) {
        Printer::error(self, message);
    }
    fn diagnostic(&self, d: &dre_core::Diagnostic) {
        self.diag(d);
    }
    fn note(&self, verb: &str, text: &str) {
        self.detail(Tone::Note, verb, text);
    }
    fn start_logs(&self) {
        self.log_runs();
    }
    fn params(&self, params: &serde_json::Value) {
        self.log_params(params);
    }
    fn timeout(&self, timeout: Option<std::time::Duration>) {
        crate::signals::set_timeout(timeout);
    }
}

impl dre_core::engine::EventSink for Printer {
    fn event(&mut self, e: RunEvent) {
        match e {
            RunEvent::Targets(t) => self.on_targets(&t),
            RunEvent::Plan(n) => self.on_plan(n),
            RunEvent::BindingStart { report, set } => self.on_binding_start(&report, set.as_deref()),
            RunEvent::BindingLog(path) => self.on_binding_log(&path),
            RunEvent::BindingVars {
                schedule,
                schedule_vars,
                vars,
            } => self.on_binding_vars(schedule.as_deref(), schedule_vars.as_ref(), &vars),
            RunEvent::Step {
                level,
                verb,
                detail,
                elapsed,
            } => self.on_step(level, &verb, &detail, elapsed),
            RunEvent::Warn(msg) => self.on_warn(&msg),
            RunEvent::BindingEnd(o) => self.on_binding_end(&o),
            RunEvent::Compiled(p) => self.on_compiled(&p),
            RunEvent::Message(m) => self.on_message(&m),
            RunEvent::PluginLog(plugin, line) => self.on_plugin_log(&plugin, &line),
            RunEvent::Sql(label, sql) => self.on_sql(&label, &sql),
            RunEvent::Running { label, running } => self.on_running(&label, running),
            RunEvent::Rows(p) => self.on_rows(&p),
            RunEvent::Block(events) => {
                for e in events {
                    self.event(e);
                }
            }
        }
    }
}

/// How each run event shows: on the terminal, as a JSON line, and in the run's log file.
impl Printer {
    fn on_targets(&mut self, t: &dre_core::run::RunTargets) {
        self.with(|i| {
            if i.format == LogFormat::Json {
                i.file_log("INFO", &format!("Target {}", t.line()));
                i.json(json!({"event": "targets", "target": t}));
            } else {
                i.line(Tone::Note, "Target", &t.line(), Level::Info);
            }
        });
        if let Some(w) = t.mismatch() {
            self.on_warn(&w);
        }
    }

    fn on_plan(&mut self, bindings: usize) {
        self.with(|i| {
            i.started = Instant::now();
            let text = format!("{bindings} Binding{}", if bindings == 1 { "" } else { "s" });
            i.file_log("INFO", &format!("Running {text}"));
            if i.format == LogFormat::Json {
                i.json(json!({"event": "plan", "bindings": bindings}));
                return;
            }
            if i.verbosity != Verbosity::Quiet {
                i.print(Tone::Note, "Running", &text);
            }
            if i.verbosity != Verbosity::Quiet && std::io::stderr().is_terminal() && bindings > 0 {
                let bar = i.multi().add(ProgressBar::new(bindings as u64));
                bar.set_style(
                    ProgressStyle::with_template(
                        "{spinner:.green} [{bar:28.green/dim}] {pos}/{len} {wide_msg}",
                    )
                    .expect("the progress template is valid")
                    .progress_chars("█▉░"),
                );
                bar.enable_steady_tick(Duration::from_millis(100));
                i.bar = Some(bar);
            }
        })
    }

    fn on_running(&mut self, label: &str, running: bool) {
        self.with(|i| {
            if running {
                i.running.push(label.to_string());
            } else {
                i.running.retain(|l| l != label);
            }
            if let Some(b) = &i.bar {
                b.set_message(format!("running {}", i.running.join(", ")));
            }
        })
    }

    fn on_binding_start(&mut self, report: &str, set: Option<&str>) {
        self.with(|i| {
            i.seen_binding = true;
            i.in_binding = true;
            i.pending.clear();
            i.current = label(report, set);
            i.file_log("INFO", "Started");
            if let Some(b) = i.bar.as_ref().filter(|_| i.running.is_empty()) {
                b.set_message(i.current.clone());
            }
            if i.format == LogFormat::Json {
                i.json(json!({"event": "binding_start", "report": report, "set": set}));
            } else if i.verbosity == Verbosity::Debug {
                let text = i.current.clone();
                i.print(Tone::Note, "Started", &text);
            }
        })
    }

    fn on_step(&mut self, level: Level, verb: &str, detail: &str, elapsed: Option<Duration>) {
        self.with(|i| {
            // The bar shows the latest step, cut to one line: a wrapped line can't be redrawn.
            if level == Level::Info
                && let Some(b) = i.bar.as_ref().filter(|_| i.running.is_empty())
            {
                b.set_message(one_line(&format!("{} · {verb} {detail}", i.current), 60));
            }
            let text = match elapsed {
                Some(d) => format!("{} {detail}", i.paint(DIM, &timing(d))),
                None => detail.to_string(),
            };
            if i.format == LogFormat::Json {
                i.file_log("DEBUG", &format!("{verb} {detail}"));
                if i.shows(level) {
                    let ms = elapsed.map(|d| d.as_millis() as u64);
                    i.json(json!({"event": "step", "binding": i.current, "verb": verb, "message": detail, "elapsed_ms": ms}));
                }
                return;
            }
            i.line(Tone::Step, verb, &text, level);
        })
    }

    fn on_warn(&mut self, msg: &str) {
        self.with(|i| {
            i.file_log("WARN", msg);
            if i.format == LogFormat::Json {
                i.json(json!({"event": "warning", "binding": i.current, "message": msg}));
            } else if i.verbosity != Verbosity::Quiet {
                i.print(Tone::Warn, "Warning", msg);
            }
        })
    }

    fn on_binding_log(&mut self, path: &Path) {
        self.with(|i| {
            if !i.run_logs {
                return;
            }
            if let Some(mut f) = LogFile::open(path) {
                for line in i.header.iter().chain(i.pending.iter()) {
                    f.write(line);
                }
                i.pending.clear();
                i.log = Some(f);
            }
        })
    }

    fn on_binding_vars(
        &mut self,
        schedule: Option<&str>,
        schedule_vars: Option<&serde_json::Map<String, serde_json::Value>>,
        vars: &serde_json::Map<String, serde_json::Value>,
    ) {
        self.with(|i| {
            if let Some(name) = schedule {
                let schedule_values = schedule_vars.cloned().unwrap_or_default();
                i.file_log(
                    "INFO",
                    &format!("Schedule {name} vars {}", protected_json(&i.secrets, &schedule_values)),
                );
            }
            let variables = protected_json(&i.secrets, vars);
            i.file_log("INFO", &format!("Vars {variables}"));
            if i.format == LogFormat::Json {
                i.json(json!({"event": "binding_vars", "schedule": schedule, "schedule_vars": schedule_vars, "vars": vars}));
            } else if i.shows(Level::Debug) {
                let text = i.paint(DIM, &variables);
                i.print(Tone::Note, "Vars", &text);
            }
        })
    }

    fn on_compiled(&mut self, p: &dre_core::run::BindingPlan) {
        self.with(|i| {
            for f in &p.compiled {
                i.file_log("INFO", &format!("Compiled {}", f.display()));
            }
            if i.format == LogFormat::Json {
                i.json(json!({"event": "compiled", "plan": p}));
            } else {
                for f in &p.compiled {
                    i.print(Tone::Good, "Compiled", &f.display().to_string());
                }
            }
        })
    }

    fn on_message(&mut self, m: &dre_core::run::ShownMessage) {
        self.with(|i| {
            i.file_log("INFO", &format!("Message {}\n{}", m.title, m.text));
            if i.format == LogFormat::Json {
                i.json(json!({"event": "message", "message": m}));
                return;
            }
            let head = match &m.output {
                Some(n) => format!("{} (output `{n}`)", m.title),
                None => m.title.clone(),
            };
            i.print(Tone::Good, "Message", &head);
            let body: String = m.text.lines().map(|l| format!("  {l}\n")).collect();
            i.print(Tone::Note, "", &format!("\n{}", body.trim_end()));
            for n in &m.notes {
                i.print(Tone::Note, "Note", n);
            }
        })
    }

    fn on_binding_end(&mut self, o: &BindingOutcome) {
        self.with(|i| {
            let name = label(&o.report, o.set.as_deref());
            let (tone, verb) = match o.status {
                Status::Error => (Tone::Bad, "Failed"),
                Status::Success => (Tone::Good, "Succeeded"),
                Status::DryRun => (Tone::Good, "Compiled"),
                Status::Checked => (Tone::Good, "Checked"),
                Status::Cancelled => (Tone::Bad, "Cancelled"),
                Status::TimedOut => (Tone::Bad, "Timed out"),
            };
            if matches!(o.status, Status::Error | Status::Cancelled | Status::TimedOut) {
                i.failed += 1;
            } else {
                i.succeeded += 1;
            }
            if let Some(b) = &i.bar {
                b.inc(1);
            }
            let detail = match &o.error {
                Some(e) => e.clone(),
                None => o.summary.clone(),
            };
            i.file_log(
                if o.error.is_some() { "ERROR" } else { "INFO" },
                &format!("{verb} in {} {detail}", fmt_secs(o.elapsed.as_secs_f64())),
            );
            i.current.clear();
            i.log = None;
            i.in_binding = false;
            if i.format == LogFormat::Json {
                i.json(json!({
                    "event": "binding_end", "report": o.report, "set": o.set, "status": o.status,
                    "elapsed_ms": o.elapsed.as_millis() as u64, "error": o.error,
                    "error_code": o.error_code, "error_kind": o.error_code.as_ref().map(|c| c.kind()), "summary": o.summary,
                    "files": o.files, "schedule": o.schedule, "schedule_vars": o.schedule_vars, "vars": o.vars,
                    "timezone": o.timezone,
                }));
                return;
            }
            if i.verbosity == Verbosity::Quiet
                && !matches!(o.status, Status::Error | Status::Cancelled | Status::TimedOut)
            {
                return;
            }
            let name = i.paint(Style::new().effects(Effects::BOLD), &name);
            let text = format!("{} {name}  {detail}", i.paint(DIM, &timing(o.elapsed)));
            i.print(tone, verb, text.trim_end());
        })
    }

    pub fn choose_set(&self, report: &str, sets: &[String]) -> Result<Option<String>, String> {
        let (multi, bar) = self.with(|i| (i.multi.clone(), i.bar.clone()));
        let ask = || -> Result<Option<String>, String> {
            eprintln!("Report `{report}` has several Sets:");
            for (i, s) in sets.iter().enumerate() {
                eprintln!("  {}) {s}", i + 1);
            }
            eprintln!("  a) all");
            loop {
                eprint!("Which one? ");
                let _ = std::io::stderr().flush();
                let mut line = String::new();
                if std::io::stdin().read_line(&mut line).map_err(|e| e.to_string())? == 0 {
                    return Err("no Set chosen".into());
                }
                let answer = line.trim();
                if answer == "a" || answer == "all" {
                    return Ok(None);
                }
                if let Some(set) = answer
                    .parse::<usize>()
                    .ok()
                    .and_then(|n| sets.get(n.checked_sub(1)?))
                {
                    return Ok(Some(set.clone()));
                }
                if sets.iter().any(|s| s == answer) {
                    return Ok(Some(answer.to_string()));
                }
            }
        };
        match (multi, bar) {
            (Some(m), _) => m.suspend(ask),
            (None, Some(b)) => b.suspend(ask),
            (None, None) => ask(),
        }
    }

    fn on_sql(&mut self, label: &str, sql: &str) {
        let body: String = sql.trim_end().lines().map(|l| format!("\n    {l}")).collect();
        self.with(|i| i.file_log("DEBUG", &format!("SQL {label}:{body}")));
    }

    pub fn on_rows(&mut self, progress: &dre_core::run::RowProgress) {
        self.with(|i| i.rows(progress));
    }

    fn on_plugin_log(&mut self, plugin: &str, line: &str) {
        self.with(|i| {
            // `info: ...` lines are for the person (e.g. waiting for a warehouse to start).
            if let Some(msg) = line.strip_prefix("info: ") {
                i.line(Tone::Note, "Waiting", &format!("[{plugin}] {msg}"), Level::Info);
                return;
            }
            // `warning: ...` lines are too (a message cut short to fit the service).
            if let Some(msg) = line.strip_prefix("warning: ") {
                i.line(Tone::Warn, "Warning", &format!("[{plugin}] {msg}"), Level::Info);
                return;
            }
            // `debug: ...` is a plugin's `log` message at debug; anything else is a raw stderr line
            // (a panic, a native driver), kept in the log file at WARN.
            let (level, line) = match line.strip_prefix("debug: ") {
                Some(rest) => ("DEBUG", rest),
                None => ("WARN", line),
            };
            i.file_log(level, &format!("[{plugin}] {line}"));
            if !i.shows(Level::Debug) {
                return;
            }
            if i.format == LogFormat::Json {
                i.json(json!({"event": "plugin_log", "plugin": plugin, "message": line}));
            } else {
                let text = i.paint(DIM, &format!("[{plugin}] {line}"));
                i.print(Tone::Note, "Plugin", &text);
            }
        })
    }
}
