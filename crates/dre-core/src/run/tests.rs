//! The run path in-process, through the plugin host interface: scripted plugins from
//! [`MemoryHost`] instead of child processes, one [`Context`] per run.

use std::sync::Arc;
use std::time::Duration;

use arrow::array::{Int64Array, RecordBatch, StringArray};
use arrow::datatypes::{DataType, Field, Schema};
use dre_protocol::Kind;
use tempfile::TempDir;

use super::*;
use crate::Env;
use crate::context::Context;
use crate::plugin_host::memory::{Call, MemoryHost, Script};

/// Records what a run reports.
#[derive(Default)]
struct Recorder {
    warnings: Vec<String>,
}

impl Ui for Recorder {
    fn step(&mut self, _level: Level, _verb: &str, _detail: &str, _elapsed: Option<Duration>) {}
    fn warn(&mut self, msg: &str) {
        self.warnings.push(msg.to_string());
    }
    fn choose_set(&mut self, _report: &str, _sets: &[String]) -> Result<Option<String>, String> {
        Ok(None)
    }
    fn plugin_log(&self) -> LogSink {
        Arc::new(|_, _| {})
    }
}

const PROFILES: &str = "connections:\n  warehouse:\n    targets:\n      dev: {type: mem}\n\
destinations:\n  outbox:\n    targets:\n      dev: {type: drop}\n\
\x20 archive:\n    targets:\n      dev: {type: shelf}\n";

/// `select 'acme' as client, 12 as total`.
fn summary() -> RecordBatch {
    let schema = Arc::new(Schema::new(vec![
        Field::new("client", DataType::Utf8, false),
        Field::new("total", DataType::Int64, false),
    ]));
    RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from(vec!["acme"])),
            Arc::new(Int64Array::from(vec![12])),
        ],
    )
    .unwrap()
}

/// Two members, 101 and 102.
fn members() -> RecordBatch {
    let schema = Arc::new(Schema::new(vec![
        Field::new("member_id", DataType::Int64, false),
        Field::new("name", DataType::Utf8, false),
    ]));
    RecordBatch::try_new(
        schema,
        vec![
            Arc::new(Int64Array::from(vec![101, 102])),
            Arc::new(StringArray::from(vec!["Ann", "Bob"])),
        ],
    )
    .unwrap()
}

/// The source, csv format and destination every test uses; `destination` scripts the last.
fn host(destination: Script) -> MemoryHost {
    MemoryHost::new()
        .plugin(
            Kind::Source,
            "mem",
            Script::new(&[dre_protocol::CAP_SESSIONS])
                .rows("select 'acme' as client, 12 as total", summary())
                .rows("select * from members", members()),
        )
        .plugin(Kind::Format, "csv", Script::new(&[]))
        .plugin(Kind::Destination, "drop", destination)
        .plugin(Kind::Destination, "shelf", Script::new(&[]))
}

/// A project with one report, `pack`, whose YAML is `report`.
fn project(report: &str, ctx: &Context) -> (TempDir, Project) {
    let dir = tempfile::tempdir().unwrap();
    let files = [
        ("dre_project.yml", "name: packs\ndefault_profile: warehouse\n"),
        ("profiles.yml", PROFILES),
        ("reports/pack/pack.yml", report),
        ("reports/pack/summary.sql", "select 'acme' as client, 12 as total"),
        ("reports/pack/members.sql", "select * from members"),
    ];
    for (path, text) in files {
        let p = dir.path().join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
    let (project, diags) = crate::project::load_with(dir.path(), &Default::default(), ctx);
    assert!(!diags.has_errors(), "{:?}", diags.sorted());
    (dir, project.unwrap())
}

fn context(host: &MemoryHost) -> Context {
    Context::new(Env::default(), Arc::new(host.clone()))
}

fn options() -> RunOptions {
    RunOptions {
        // The report's SQL files would run as reports of their own.
        selector: Some("pack".into()),
        date: chrono::NaiveDate::from_ymd_opt(2026, 1, 25),
        ..RunOptions::default()
    }
}

fn run_once(ctx: &Context, project: &Project, opts: &RunOptions) -> RunSummary {
    run(ctx, project, opts, &mut Recorder::default())
}

const ONE_FILE: &str = "queries: [summary]\noutput:\n  name: detail\n  format: csv\n\
    \x20 destination: {profile: outbox, path: out/detail.csv}\n";

#[test]
fn a_query_is_formatted_and_delivered() {
    let host = host(Script::new(&[]));
    let ctx = context(&host);
    let (_dir, project) = project(ONE_FILE, &ctx);
    let summary = run_once(&ctx, &project, &options());
    assert!(!summary.failed(), "{:?}", summary.outcomes);
    assert_eq!(summary.outcomes[0].status, Status::Success);
    let calls = host.calls();
    let shown: Vec<&str> = calls
        .iter()
        .map(|c| c.split_whitespace().take(2).collect::<Vec<_>>())
        .map(|c| match c.as_slice() {
            ["source/mem", "Execute"] => "execute",
            ["format/csv", "Write"] => "write",
            ["destination/drop", "Deliver"] => "deliver",
            _ => "",
        })
        .filter(|c| !c.is_empty())
        .collect();
    assert_eq!(shown, ["execute", "write", "deliver"]);
    assert!(
        calls.iter().any(|c| c.ends_with("Deliver out/detail.csv")),
        "{calls:?}"
    );
    // The format wrote the file the run reports.
    let file = &summary.outcomes[0].files[0];
    assert_eq!(std::fs::read_to_string(file).unwrap(), "summary: 1 rows\n");
}

const PER_ROW: &str = "queries: [members]\noutput:\n  name: letter\n  format: csv\n\
    \x20 for_each: {query: members, key: member_id}\n\
    \x20 destination: {profile: outbox, path: \"out/{{ each.member_id }}.csv\"}\n";

#[test]
fn a_retry_delivers_only_the_rows_whose_destination_failed() {
    // Rows deliver on concurrent workers, in no set order: fail member 101's letter by its
    // path, never by call order.
    let host = host(Script::new(&[]).fail_on(Call::Deliver, "out/101.csv", "the mailbox is full"));
    let ctx = context(&host);
    let (dir, project) = project(PER_ROW, &ctx);
    let first = run_once(&ctx, &project, &options());
    assert!(first.failed());
    let delivered = |host: &MemoryHost| -> Vec<String> {
        let mut calls: Vec<String> = host
            .calls()
            .into_iter()
            .filter(|c| c.starts_with("destination/drop Deliver"))
            .collect();
        calls.sort();
        calls
    };
    assert_eq!(
        delivered(&host),
        [
            "destination/drop Deliver out/101.csv",
            "destination/drop Deliver out/102.csv"
        ]
    );
    let runs = crate::runs::BindingRuns::new(&dir.path().join("target/run/pack/default"));
    let run_id = runs.list().into_iter().last().unwrap();
    let retry = RunOptions {
        retry_failed: Some(run_id),
        ..options()
    };
    // The mailbox has room again.
    let retry_host = self::host(Script::new(&[]));
    let second = run_once(&context(&retry_host), &project, &retry);
    assert!(!second.failed(), "{:?}", second.outcomes);
    // Only member 101's letter is sent again.
    assert_eq!(delivered(&retry_host), ["destination/drop Deliver out/101.csv"]);
}

#[test]
fn a_held_delivery_waits_for_its_release() {
    let host = host(Script::new(&[]));
    let ctx = context(&host);
    let (dir, project) = project(
        "queries: [summary]\noutput:\n  name: detail\n  format: csv\n\
         \x20 destination: {profile: outbox, path: out/detail.csv, hold: true}\n",
        &ctx,
    );
    let summary = run_once(&ctx, &project, &options());
    assert!(!summary.failed(), "{:?}", summary.outcomes);
    assert!(!host.calls().iter().any(|c| c.contains("Deliver")));
    let runs = crate::runs::BindingRuns::new(&dir.path().join("target/run/pack/default"));
    let run_id = runs.list().into_iter().last().unwrap();
    let (tx, _rx) = std::sync::mpsc::channel();
    let released = crate::release::release(
        &ctx,
        &project,
        &crate::release::ReleaseOptions {
            report: "pack".into(),
            binding: "default".into(),
            run_id,
            only_keys: None,
            exclude_keys: None,
            by: "tester".into(),
            rate: None,
            cancel: CancelToken::new(),
        },
        &Events::new(tx),
    )
    .unwrap();
    assert!(!released.failed());
    assert!(
        host.calls()
            .iter()
            .any(|c| c == "destination/drop Deliver out/detail.csv")
    );
}

#[test]
fn a_zip_entry_delivers_one_zip_of_the_files() {
    let host = host(Script::new(&[]));
    let ctx = context(&host);
    let (_dir, project) = project(
        "queries: [summary]\noutput:\n  name: detail\n  format: csv\n\
         \x20 destination: {profile: outbox, path: out/detail.zip, zip: true}\n",
        &ctx,
    );
    let summary = run_once(&ctx, &project, &options());
    assert!(!summary.failed(), "{:?}", summary.outcomes);
    let deliveries: Vec<String> = host
        .calls()
        .into_iter()
        .filter(|c| c.starts_with("destination/drop Deliver"))
        .collect();
    assert_eq!(deliveries, ["destination/drop Deliver out/detail.zip"]);
}

/// A run whose query takes `delay`, on its own host and context.
fn slow_run(delay: Duration) -> (MemoryHost, Context, TempDir, Project) {
    let host = MemoryHost::new()
        .plugin(
            Kind::Source,
            "mem",
            Script::new(&[dre_protocol::CAP_SESSIONS])
                .rows("select 'acme' as client, 12 as total", summary())
                .delay(Call::Execute, delay),
        )
        .plugin(Kind::Format, "csv", Script::new(&[]))
        .plugin(Kind::Destination, "drop", Script::new(&[]));
    let ctx = context(&host);
    let (dir, project) = project(ONE_FILE, &ctx);
    (host, ctx, dir, project)
}

#[test]
fn cancelling_a_run_stops_its_plugins_and_delivers_nothing() {
    let (host, ctx, _dir, project) = slow_run(Duration::from_secs(30));
    let opts = options();
    let cancel = opts.cancel.clone();
    let stopper = ctx.host().clone();
    let started = std::time::Instant::now();
    let summary = std::thread::scope(|s| {
        s.spawn(|| {
            std::thread::sleep(Duration::from_millis(200));
            cancel.cancel();
            stopper.cancel();
        });
        run_once(&ctx, &project, &opts)
    });
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(host.cancelled());
    assert_eq!(summary.cancelled, Some(crate::engine::CancelReason::Interrupt));
    assert_ne!(summary.outcomes[0].status, Status::Success);
    assert!(!host.calls().iter().any(|c| c.contains("Deliver")));
}

#[test]
fn two_runs_in_one_process_share_no_cancellation_or_secrets() {
    let (slow_host, slow_ctx, _a, slow_project) = slow_run(Duration::from_secs(30));
    let (quick_host, quick_ctx, _b, quick_project) = slow_run(Duration::from_millis(300));
    slow_ctx
        .secrets()
        .register_values(["only-in-the-slow-run".to_string()]);
    let slow_opts = options();
    let cancel = slow_opts.cancel.clone();
    let (slow, quick) = std::thread::scope(|s| {
        let slow = s.spawn(|| run_once(&slow_ctx, &slow_project, &slow_opts));
        let quick = s.spawn(|| run_once(&quick_ctx, &quick_project, &options()));
        std::thread::sleep(Duration::from_millis(100));
        cancel.cancel();
        slow_ctx.host().cancel();
        (slow.join().unwrap(), quick.join().unwrap())
    });
    assert!(slow_host.cancelled() && !quick_host.cancelled());
    assert_eq!(slow.cancelled, Some(crate::engine::CancelReason::Interrupt));
    assert!(!quick.failed(), "{:?}", quick.outcomes);
    assert!(
        quick_host
            .calls()
            .iter()
            .any(|c| c == "destination/drop Deliver out/detail.csv")
    );
    assert_eq!(slow_ctx.secrets().mask("only-in-the-slow-run"), "*****");
    assert_eq!(
        quick_ctx.secrets().mask("only-in-the-slow-run"),
        "only-in-the-slow-run"
    );
}

#[test]
fn each_destination_gets_its_own_rendered_profile_when_some_render_per_row() {
    let host = host(Script::new(&[]));
    let ctx = context(&host);
    // The per-row output's destination renders per row, so the run renders only the others:
    // each must still find its own profile.
    let (_dir, project) = project(
        "queries: [members, summary]\noutput:\n\
         \x20 - name: letter\n    format: csv\n    queries: [members]\n\
         \x20   for_each: {query: members, key: member_id}\n\
         \x20   destination: {profile: outbox, path: \"letters/{{ each.member_id }}.csv\"}\n\
         \x20 - name: detail\n    format: csv\n    queries: [summary]\n\
         \x20   destination:\n\
         \x20     - {profile: \"{{ 'archive' }}\", path: shelf/detail.csv}\n\
         \x20     - {profile: outbox, path: out/detail.csv}\n",
        &ctx,
    );
    let summary = run_once(&ctx, &project, &options());
    assert!(!summary.failed(), "{:?}", summary.outcomes);
    let mut deliveries: Vec<String> = host
        .calls()
        .into_iter()
        .filter(|c| c.contains(" Deliver "))
        .collect();
    deliveries.sort();
    assert_eq!(
        deliveries,
        [
            "destination/drop Deliver letters/101.csv",
            "destination/drop Deliver letters/102.csv",
            "destination/drop Deliver out/detail.csv",
            "destination/shelf Deliver shelf/detail.csv",
        ]
    );
}
