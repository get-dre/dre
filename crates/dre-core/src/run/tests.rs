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

/// Every event a run sends, blocks unpacked.
#[derive(Default)]
struct Recorder {
    events: Vec<RunEvent>,
}

impl EventSink for Recorder {
    fn event(&mut self, event: RunEvent) {
        match event {
            RunEvent::Block(events) => {
                for e in events {
                    self.event(e);
                }
            }
            e => self.events.push(e),
        }
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
    project_with_profiles(report, PROFILES, ctx)
}

fn project_with_profiles(report: &str, profiles: &str, ctx: &Context) -> (TempDir, Project) {
    let dir = tempfile::tempdir().unwrap();
    let files = [
        (
            "dre_project.yml",
            "name: packs\ndefault_profile: warehouse\nallow_unmanaged: true\n",
        ),
        ("profiles.yml", profiles),
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
        selector: Some("report:pack".into()),
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
fn a_row_whose_job_panics_fails_alone() {
    let host = host(Script::new(&[]).panic_on(Call::Deliver, "out/101.csv"));
    let ctx = context(&host);
    let (dir, project) = project(PER_ROW, &ctx);
    let summary = run_once(&ctx, &project, &options());
    assert!(summary.failed());
    assert!(
        host.calls()
            .contains(&"destination/drop Deliver out/102.csv".to_string())
    );
    let fe = &results_json(&dir)["output_results"][0]["for_each"];
    assert_eq!(
        (fe["succeeded"].as_u64(), fe["failed"].as_u64()),
        (Some(1), Some(1)),
        "{fe}"
    );
    let rows = std::fs::read_to_string(dir.path().join(fe["sidecar"].as_str().unwrap())).unwrap();
    assert!(rows.contains("internal error"), "{rows}");
}

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
        mode: Mode {
            results: Results::Retry(run_id),
            ..Mode::default()
        },
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
    let (summary, cancelled_at) = std::thread::scope(|s| {
        let stop = s.spawn(|| {
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while calls_of(&host, "Execute").is_empty() {
                assert!(std::time::Instant::now() < deadline, "query never started");
                std::thread::sleep(Duration::from_millis(5));
            }
            let cancelled_at = std::time::Instant::now();
            cancel.cancel();
            stopper.cancel();
            cancelled_at
        });
        let summary = run_once(&ctx, &project, &opts);
        (summary, stop.join().unwrap())
    });
    assert!(cancelled_at.elapsed() < Duration::from_secs(10));
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

/// A run and every event it sent.
fn run_recorded(ctx: &Context, project: &Project, opts: &RunOptions) -> (RunSummary, Vec<RunEvent>) {
    let mut recorder = Recorder::default();
    let summary = run(ctx, project, opts, &mut recorder);
    (summary, recorder.events)
}

/// The calls of one kind, `"<plugin> <call>"` without details, sorted.
fn calls_of(host: &MemoryHost, call: &str) -> Vec<String> {
    let mut calls: Vec<String> = host
        .calls()
        .into_iter()
        .filter(|c| c.split_whitespace().nth(1) == Some(call))
        .collect();
    calls.sort();
    calls
}

fn with_mode(mode: Mode) -> RunOptions {
    RunOptions { mode, ..options() }
}

fn results_json(dir: &TempDir) -> serde_json::Value {
    let runs = crate::runs::BindingRuns::new(&dir.path().join("target/run/pack/default"));
    let id = runs.list().into_iter().last().expect("a run folder");
    let text = std::fs::read_to_string(runs.run_dir(&id).join("run_results.json")).unwrap();
    serde_json::from_str(&text).unwrap()
}

#[test]
fn events_tell_the_run_in_order() {
    let host = host(Script::new(&[]));
    let ctx = context(&host);
    let (_dir, project) = project(ONE_FILE, &ctx);
    let (summary, events) = run_recorded(&ctx, &project, &options());
    assert!(!summary.failed(), "{:?}", summary.outcomes);
    let names: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            RunEvent::Targets(_) => Some("targets"),
            RunEvent::Plan(1) => Some("plan"),
            RunEvent::BindingStart { .. } => Some("start"),
            RunEvent::BindingEnd(_) => Some("end"),
            _ => None,
        })
        .collect();
    assert_eq!(names, ["targets", "plan", "start", "end"]);
}

#[test]
fn compiling_renders_the_sql_and_runs_nothing() {
    let host = host(Script::new(&[]));
    let ctx = context(&host);
    let (dir, project) = project(ONE_FILE, &ctx);
    let (summary, events) = run_recorded(&ctx, &project, &with_mode(Mode::compile()));
    assert_eq!(
        summary.outcomes[0].status,
        Status::DryRun,
        "{:?}",
        summary.outcomes
    );
    assert!(calls_of(&host, "Execute").is_empty() && calls_of(&host, "Deliver").is_empty());
    let compiled = dir.path().join("target/compiled/pack/default/summary.sql");
    assert_eq!(
        std::fs::read_to_string(compiled).unwrap(),
        "select 'acme' as client, 12 as total"
    );
    let plan = events.iter().find_map(|e| match e {
        RunEvent::Compiled(p) => Some(p),
        _ => None,
    });
    assert_eq!(
        plan.unwrap().destinations[0].path.as_deref(),
        Some("out/detail.csv")
    );
    // A compile makes no run folder.
    assert!(!dir.path().join("target/run/pack/default/runs").exists());
}

#[test]
fn a_compile_whose_template_needs_the_database_says_only_a_run_can_check_it() {
    let host = MemoryHost::new()
        .plugin(
            Kind::Source,
            "mem",
            Script::new(&[dre_protocol::CAP_SESSIONS]).fail(Call::Execute, 1, "no such table: staging"),
        )
        .plugin(Kind::Format, "csv", Script::new(&[]))
        .plugin(Kind::Destination, "drop", Script::new(&[]));
    let ctx = context(&host);
    let (dir, project) = project(ONE_FILE, &ctx);
    std::fs::write(
        dir.path().join("reports/pack/summary.sql"),
        "{% set n = run_query('select count(*) from staging') %}select 1",
    )
    .unwrap();
    let summary = run_once(&ctx, &project, &with_mode(Mode::compile()));
    let o = &summary.outcomes[0];
    assert_eq!(o.status, Status::Error);
    assert_eq!(o.error_code, Some(Code::CompileNeedsRun.into()), "{:?}", o.error);
}

#[test]
fn checking_runs_no_statement_and_delivers_nothing() {
    let host = MemoryHost::new()
        .plugin(
            Kind::Source,
            "mem",
            Script::new(&[dre_protocol::CAP_SESSIONS, dre_protocol::CAP_CHECK]),
        )
        .plugin(Kind::Format, "csv", Script::new(&[]))
        .plugin(Kind::Destination, "drop", Script::new(&[]));
    let ctx = context(&host);
    let (_dir, project) = project(ONE_FILE, &ctx);
    let summary = run_once(&ctx, &project, &with_mode(Mode::check()));
    assert_eq!(
        summary.outcomes[0].status,
        Status::Checked,
        "{:?}",
        summary.outcomes
    );
    assert!(calls_of(&host, "Execute").is_empty() && calls_of(&host, "Write").is_empty());
}

#[test]
fn a_preview_formats_but_delivers_nothing_and_records_the_preview() {
    let host = host(Script::new(&[]));
    let ctx = context(&host);
    let (dir, project) = project(ONE_FILE, &ctx);
    let summary = run_once(
        &ctx,
        &project,
        &with_mode(Mode {
            preview: Some(10),
            ..Mode::default()
        }),
    );
    assert!(!summary.failed(), "{:?}", summary.outcomes);
    assert_eq!(calls_of(&host, "Write").len(), 1);
    assert!(calls_of(&host, "Deliver").is_empty());
    let r = results_json(&dir);
    assert_eq!(
        (r["preview"].as_bool(), r["row_limit"].as_u64()),
        (Some(true), Some(10))
    );
    assert_eq!(r["delivery"], "preview: not delivered");
    // A preview leaves the schema snapshot alone.
    assert!(
        !dir.path()
            .join("target/schema/pack/default/last_success.json")
            .exists()
    );
}

#[test]
fn no_deliver_keeps_the_output_and_needs_no_destination_entry() {
    let host = host(Script::new(&[]));
    let ctx = context(&host);
    let (dir, _) = project(ONE_FILE, &ctx);
    // The destination has no entry for this target: `--no-deliver` doesn't need one.
    std::fs::write(
        dir.path().join("profiles.yml"),
        PROFILES.replace("dev: {type: drop}", "prod: {type: drop}"),
    )
    .unwrap();
    let (_, project) = (
        0,
        crate::project::load_with(dir.path(), &Default::default(), &ctx)
            .0
            .unwrap(),
    );
    let summary = run_once(
        &ctx,
        &project,
        &with_mode(Mode {
            delivery: DeliveryMode::Keep,
            ..Mode::default()
        }),
    );
    assert!(!summary.failed(), "{:?} {:?}", summary.error, summary.outcomes);
    assert!(calls_of(&host, "Deliver").is_empty());
    assert_eq!(
        results_json(&dir)["delivery"],
        "--no-deliver: output kept locally"
    );
    // Delivering needs the entry: nothing runs.
    let refused = run_once(&ctx, &project, &options());
    assert_eq!(
        refused.error.map(|e| e.code),
        Some(Code::MissingTargetEntry.into())
    );
}

#[test]
fn a_replay_uses_the_saved_results_and_runs_no_query() {
    let host = host(Script::new(&[]));
    let ctx = context(&host);
    let (_dir, project) = project(ONE_FILE, &ctx);
    let save = RunOptions {
        output_json: Some("saved".into()),
        mode: Mode {
            delivery: DeliveryMode::Keep,
            ..Mode::default()
        },
        ..options()
    };
    assert!(!run_once(&ctx, &project, &save).failed());
    let replay_host = self::host(Script::new(&[]));
    let replay = with_mode(Mode {
        results: Results::Replay("saved".into()),
        delivery: DeliveryMode::Keep,
        ..Mode::default()
    });
    let summary = run_once(&context(&replay_host), &project, &replay);
    assert!(!summary.failed(), "{:?}", summary.outcomes);
    assert!(calls_of(&replay_host, "Execute").is_empty());
    assert_eq!(calls_of(&replay_host, "Write").len(), 1);
}

#[test]
fn hold_holds_every_destination_of_the_run() {
    let host = host(Script::new(&[]));
    let ctx = context(&host);
    let (dir, project) = project(ONE_FILE, &ctx);
    let summary = run_once(
        &ctx,
        &project,
        &with_mode(Mode {
            delivery: DeliveryMode::Hold,
            ..Mode::default()
        }),
    );
    assert!(!summary.failed(), "{:?}", summary.outcomes);
    assert!(calls_of(&host, "Deliver").is_empty());
    assert_eq!(results_json(&dir)["deliveries"][0]["status"], "held");
}

#[test]
fn an_output_whose_when_is_false_is_skipped_unformatted() {
    let host = host(Script::new(&[]));
    let ctx = context(&host);
    let (dir, project) = project(
        "queries: [summary]\noutput:\n  name: detail\n  format: csv\n  when: \"results.summary.row_count > 5\"\n\
         \x20 destination: {profile: outbox, path: out/detail.csv}\n",
        &ctx,
    );
    let summary = run_once(&ctx, &project, &options());
    assert!(!summary.failed(), "{:?}", summary.outcomes);
    assert!(calls_of(&host, "Write").is_empty() && calls_of(&host, "Deliver").is_empty());
    let r = results_json(&dir);
    assert_eq!(r["output_results"][0]["status"], "skipped");
    assert_eq!(r["output_results"][0]["when"], false);
}

#[test]
fn a_failed_delivery_is_recorded_with_its_code() {
    let host = host(Script::new(&[]).fail(Call::Deliver, 1, "the mailbox is full"));
    let ctx = context(&host);
    let (dir, project) = project(ONE_FILE, &ctx);
    let summary = run_once(&ctx, &project, &options());
    let o = &summary.outcomes[0];
    assert_eq!(o.status, Status::Error);
    assert_eq!(o.error_code, Some(Code::DeliveryFailed.into()));
    let r = results_json(&dir);
    assert_eq!(r["status"], "error");
    assert_eq!(r["output_results"][0]["status"], "failed");
    assert_eq!(r["deliveries"][0]["error_code"], "delivery-failed");
}

#[test]
fn an_unmanaged_report_that_writes_runs_nothing() {
    let host = host(Script::new(&[]));
    let ctx = context(&host);
    let (dir, _) = project(ONE_FILE, &ctx);
    std::fs::create_dir_all(dir.path().join("reports/loose")).unwrap();
    std::fs::write(
        dir.path().join("reports/loose/loose.sql"),
        "select 1;\ndelete from members",
    )
    .unwrap();
    let project = crate::project::load_with(dir.path(), &Default::default(), &ctx)
        .0
        .unwrap();
    let opts = RunOptions {
        selector: Some("loose".into()),
        ..options()
    };
    let summary = run_once(&ctx, &project, &opts);
    let o = &summary.outcomes[0];
    assert_eq!(o.status, Status::Error);
    assert!(
        o.error.as_deref().unwrap_or("").contains("unmanaged report"),
        "{:?}",
        o.error
    );
    // Everything renders before anything runs, so the first SELECT never ran either.
    assert!(calls_of(&host, "Execute").is_empty());
}

#[test]
fn a_selector_that_matches_nothing_is_a_typed_error() {
    let host = host(Script::new(&[]));
    let ctx = context(&host);
    let (_dir, project) = project(ONE_FILE, &ctx);
    let summary = run_once(
        &ctx,
        &project,
        &RunOptions {
            selector: Some("nothing-like-this".into()),
            ..options()
        },
    );
    let e = summary.error.clone().expect("nothing ran");
    assert_eq!(e.code, Code::InvalidSelector);
    assert_eq!(summary.exit_code(), 2);
}

/// Three reports on one connection whose entry allows three Bindings at once.
fn three_reports(single_writer: bool) -> (MemoryHost, Context, TempDir, Project) {
    let description = dre_protocol::host::Description {
        single_writer: single_writer.then(|| dre_protocol::msg::SingleWriter {
            field: "path".into(),
            except: vec![":memory:".into()],
        }),
        ..Default::default()
    };
    let host = MemoryHost::new()
        .plugin(
            Kind::Source,
            "mem",
            Script::new(&[dre_protocol::CAP_SESSIONS])
                .rows("select 'acme' as client, 12 as total", summary())
                .description(description),
        )
        .plugin(Kind::Format, "csv", Script::new(&[]))
        .plugin(Kind::Destination, "drop", Script::new(&[]));
    let ctx = context(&host);
    let (dir, _) = project(ONE_FILE, &ctx);
    std::fs::write(
        dir.path().join("profiles.yml"),
        PROFILES.replace("dev: {type: mem}", "dev: {type: mem, path: shop.db, threads: 3}"),
    )
    .unwrap();
    for r in ["a", "b"] {
        let d = dir.path().join(format!("reports/{r}"));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join(format!("{r}.yml")),
            format!("queries: [summary_{r}]\noutput:\n  format: csv\n  destination: {{profile: outbox, path: out/{r}.csv}}\n"),
        )
        .unwrap();
        std::fs::write(
            d.join(format!("summary_{r}.sql")),
            "select 'acme' as client, 12 as total",
        )
        .unwrap();
    }
    let project = crate::project::load_with(dir.path(), &Default::default(), &ctx)
        .0
        .unwrap();
    (host, ctx, dir, project)
}

/// How many workers the plan runs three Bindings on, and whether they hold their events.
fn planned_workers(ctx: &Context, project: &Project) -> (usize, bool) {
    let opts = RunOptions {
        selector: Some("report:pack a b".into()),
        ..options()
    };
    let p = plan(ctx, project, &opts, &mut Recorder::default(), &mut Vec::new()).unwrap();
    assert_eq!(p.bindings.len(), 3);
    (p.scheduler.workers(), p.at_once)
}

#[test]
fn a_connection_runs_bindings_at_once_up_to_its_threads() {
    let (_host, ctx, _dir, project) = three_reports(false);
    assert_eq!(planned_workers(&ctx, &project), (3, true));
}

#[test]
fn a_file_only_one_process_may_write_runs_one_binding_at_a_time() {
    // The source's `describe` says `path` names such a file, whatever `threads:` says.
    let (_host, ctx, _dir, project) = three_reports(true);
    assert_eq!(planned_workers(&ctx, &project), (1, false));
}

#[test]
fn bindings_at_once_all_run_and_report_in_declaration_order() {
    for _ in 0..3 {
        let (host, ctx, _dir, project) = three_reports(false);
        let opts = RunOptions {
            selector: Some("report:pack a b".into()),
            ..options()
        };
        let planned: Vec<String> = plan(&ctx, &project, &opts, &mut Recorder::default(), &mut Vec::new())
            .unwrap()
            .bindings
            .iter()
            .map(|p| p.report.name.clone())
            .collect();
        let (summary, events) = run_recorded(&ctx, &project, &opts);
        assert!(!summary.failed(), "{:?}", summary.outcomes);
        let reports: Vec<String> = summary.outcomes.iter().map(|o| o.report.clone()).collect();
        assert_eq!(reports, planned);
        assert_eq!(calls_of(&host, "Deliver").len(), 3);
        let starts = events
            .iter()
            .filter(|e| matches!(e, RunEvent::BindingStart { .. }))
            .count();
        assert_eq!(starts, 3);
    }
}

const TWO_BINDING_FILES: &str = "queries: [summary]\nsets: [{name: east}, {name: west}]\noutput:\n  - {name: first, format: csv, destination: {profile: outbox, path: 'out/{{ run.set }}/first.csv'}}\n  - {name: second, format: csv, destination: {profile: outbox, path: 'out/{{ run.set }}/second.csv'}}\n";

fn two_binding_files(destination: Script) -> (MemoryHost, Context, TempDir, Project) {
    let host = host(destination);
    let ctx = context(&host);
    let (dir, project) = project(TWO_BINDING_FILES, &ctx);
    (host, ctx, dir, project)
}

fn all_sets() -> RunOptions {
    RunOptions {
        set: Some("all".into()),
        ..options()
    }
}

#[test]
fn formats_and_destinations_are_reused_across_outputs_and_bindings() {
    let (host, ctx, _dir, project) = two_binding_files(Script::new(&[]));
    let summary = run_once(&ctx, &project, &all_sets());
    assert!(!summary.failed(), "{:?}", summary.outcomes);
    assert_eq!(calls_of(&host, "Write").len(), 4);
    assert_eq!(calls_of(&host, "Deliver").len(), 4);
    let starts = calls_of(&host, "Start");
    assert_eq!(
        starts.iter().filter(|c| c.starts_with("format/csv")).count(),
        1,
        "{starts:?}"
    );
    assert_eq!(
        starts
            .iter()
            .filter(|c| c.starts_with("destination/drop"))
            .count(),
        2,
        "{starts:?}"
    );
    let closes = calls_of(&host, "Close");
    assert_eq!(
        closes.iter().filter(|c| c.starts_with("format/csv")).count(),
        1,
        "{closes:?}"
    );
    assert_eq!(
        closes
            .iter()
            .filter(|c| c.starts_with("destination/drop"))
            .count(),
        2,
        "{closes:?}"
    );
}

#[test]
fn a_failed_destination_is_closed_and_replaced_while_formats_stay_warm() {
    let (host, ctx, _dir, project) =
        two_binding_files(Script::new(&[]).fail(Call::Deliver, 2, "mailbox full"));
    let summary = run_once(&ctx, &project, &all_sets());
    assert!(summary.failed());
    assert_eq!(summary.outcomes.len(), 2);
    assert_eq!(summary.outcomes[1].status, Status::Success);
    assert_eq!(calls_of(&host, "Deliver").len(), 4);
    let starts = calls_of(&host, "Start");
    assert_eq!(
        starts.iter().filter(|c| c.starts_with("format/csv")).count(),
        1,
        "{starts:?}"
    );
    // One describe process, the original delivery process, and its replacement.
    assert_eq!(
        starts
            .iter()
            .filter(|c| c.starts_with("destination/drop"))
            .count(),
        3,
        "{starts:?}"
    );
    assert_eq!(calls_of(&host, "Close").len(), starts.len());
}

#[test]
fn cancelling_a_reused_destination_closes_every_retained_process() {
    let host = host(Script::new(&[]).delay(Call::Deliver, Duration::from_millis(500)));
    let ctx = context(&host);
    let report = "queries: [summary]\nsets: [{name: east}, {name: west}, {name: north}]\noutput: {name: detail, format: csv, destination: {profile: outbox, path: 'out/{{ run.set }}/detail.csv'}}\n";
    let (_dir, project) = project(report, &ctx);
    let opts = RunOptions {
        set: Some("all".into()),
        ..options()
    };
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while calls_of(&host, "Deliver").len() < 2 {
                assert!(
                    std::time::Instant::now() < deadline,
                    "second delivery never started"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            opts.cancel.cancel();
            ctx.host().cancel();
        });
        let summary = run_once(&ctx, &project, &opts);
        assert_eq!(summary.cancelled, Some(crate::engine::CancelReason::Interrupt));
    });
    assert_eq!(calls_of(&host, "Deliver").len(), 2);
    let starts = calls_of(&host, "Start");
    assert_eq!(
        starts.iter().filter(|c| c.starts_with("format/csv")).count(),
        1,
        "{starts:?}"
    );
    assert_eq!(
        starts
            .iter()
            .filter(|c| c.starts_with("destination/drop"))
            .count(),
        2,
        "{starts:?}"
    );
    assert_eq!(calls_of(&host, "Close").len(), starts.len());
}

fn many_members(start: i64, count: usize) -> RecordBatch {
    RecordBatch::try_from_iter([
        (
            "member_id",
            Arc::new(Int64Array::from_iter_values(start..start + count as i64)) as arrow::array::ArrayRef,
        ),
        (
            "name",
            Arc::new(StringArray::from_iter_values(
                (0..count).map(|i| format!("Member {i}")),
            )) as arrow::array::ArrayRef,
        ),
    ])
    .unwrap()
}

#[test]
fn per_row_chunks_reuse_processes_and_match_unsorted_detail_batches() {
    let host = MemoryHost::new()
        .plugin(
            Kind::Source,
            "mem",
            Script::new(&[dre_protocol::CAP_SESSIONS])
                .batches(
                    "select * from members",
                    vec![many_members(1, 150), many_members(151, 150)],
                )
                .batches(
                    "select 'acme' as client, 12 as total",
                    vec![many_members(151, 150), many_members(1, 150)],
                ),
        )
        .plugin(Kind::Format, "csv", Script::new(&[]).record_writes())
        .plugin(Kind::Destination, "drop", Script::new(&[]));
    let ctx = context(&host);
    let report = "queries: [members, summary]\noutput: \n  - name: letter\n    format: csv\n    queries: [members, summary]\n    for_each: {query: members, key: member_id, concurrency: 1}\n    destination: {profile: outbox, path: 'out/{{ each.member_id }}.csv'}\n  - name: copy\n    format: csv\n    queries: [members, summary]\n    for_each: {query: members, key: member_id, concurrency: 1}\n    destination: {profile: outbox, path: 'copy/{{ each.member_id }}.csv'}\n";
    let (dir, project) = project(report, &ctx);
    let summary = run_once(&ctx, &project, &options());
    assert!(!summary.failed(), "{:?}", summary.outcomes);
    assert_eq!(calls_of(&host, "Write").len(), 600);
    assert_eq!(calls_of(&host, "Deliver").len(), 600);
    let row_files: Vec<std::path::PathBuf> =
        walkdir::WalkDir::new(dir.path().join("target/run/pack/default/runs"))
            .into_iter()
            .map(Result::unwrap)
            .filter(|entry| {
                entry.file_type().is_file() && entry.path().extension().is_some_and(|ext| ext == "csv")
            })
            .map(|entry| entry.into_path())
            .collect();
    assert_eq!(row_files.len(), 600);
    for file in row_files {
        assert_eq!(std::fs::read_to_string(file).unwrap(), "summary: 1 rows\n");
    }
    let writes = host.writes();
    assert_eq!(writes.len(), 600);
    // Literal keys and values straddle both 128-row job chunks and the reversed 150-row
    // source batches. A correct row count cannot hide a batch/row location mix-up.
    for (id, name) in [
        (1, "Member 0"),
        (128, "Member 127"),
        (129, "Member 128"),
        (150, "Member 149"),
        (151, "Member 0"),
        (256, "Member 105"),
        (257, "Member 106"),
        (300, "Member 149"),
    ] {
        for output in ["letter", "copy"] {
            let suffix = std::path::PathBuf::from(output).join(format!("{id}.csv"));
            let written = writes
                .iter()
                .find(|w| std::path::Path::new(&w.path).ends_with(&suffix))
                .unwrap();
            assert_eq!(written.result_set, 0);
            let keys = written
                .batch
                .column(0)
                .as_any()
                .downcast_ref::<Int64Array>()
                .unwrap();
            let names = written
                .batch
                .column(1)
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            assert_eq!(keys.value(0), id, "{}", written.path);
            assert_eq!(names.value(0), name, "{}", written.path);
        }
    }
    let starts = calls_of(&host, "Start");
    assert_eq!(
        starts.iter().filter(|c| c.starts_with("format/csv")).count(),
        1,
        "{starts:?}"
    );
    assert_eq!(
        starts
            .iter()
            .filter(|c| c.starts_with("destination/drop"))
            .count(),
        2,
        "{starts:?}"
    );
    assert_eq!(calls_of(&host, "Close").len(), starts.len());
}

#[test]
fn driver_duplicates_in_later_spool_batches_fail_before_delivery() {
    let host = MemoryHost::new()
        .plugin(
            Kind::Source,
            "mem",
            Script::new(&[dre_protocol::CAP_SESSIONS]).batches(
                "select * from members",
                vec![many_members(1, 150), many_members(150, 1)],
            ),
        )
        .plugin(Kind::Format, "csv", Script::new(&[]))
        .plugin(Kind::Destination, "drop", Script::new(&[]));
    let ctx = context(&host);
    let (_dir, project) = project(PER_ROW, &ctx);
    let summary = run_once(&ctx, &project, &options());
    assert!(summary.failed());
    assert!(
        summary.outcomes[0]
            .error
            .as_ref()
            .unwrap()
            .contains("key `150` twice")
    );
    assert!(calls_of(&host, "Write").is_empty());
    assert!(calls_of(&host, "Deliver").is_empty());
}

#[test]
fn two_destination_profiles_of_one_kind_keep_distinct_processes() {
    let host = host(Script::new(&[]));
    let ctx = context(&host);
    let report = "queries: [summary]\noutput: {name: detail, format: csv, destination: [{profile: outbox, path: out/detail.csv}, {profile: archive, path: shelf/detail.csv}]}\n";
    let (_dir, project) = project_with_profiles(report, &PROFILES.replace("type: shelf", "type: drop"), &ctx);
    let summary = run_once(&ctx, &project, &options());
    assert!(!summary.failed(), "{:?}", summary.outcomes);
    assert_eq!(calls_of(&host, "Deliver").len(), 2);
    let starts = calls_of(&host, "Start");
    // One describe process, then one delivery process per profile.
    assert_eq!(
        starts
            .iter()
            .filter(|c| c.starts_with("destination/drop"))
            .count(),
        3,
        "{starts:?}"
    );
    assert_eq!(calls_of(&host, "Close").len(), starts.len());
}

fn large_per_row(format: Script, changes: &str) -> (MemoryHost, Context, TempDir, Project) {
    let host = MemoryHost::new()
        .plugin(
            Kind::Source,
            "mem",
            Script::new(&[dre_protocol::CAP_SESSIONS]).batches(
                "select * from members",
                vec![many_members(1, 150), many_members(151, 150)],
            ),
        )
        .plugin(Kind::Format, "csv", format)
        .plugin(Kind::Destination, "drop", Script::new(&[]));
    let ctx = context(&host);
    let report = PER_ROW.replace(
        "key: member_id}",
        &format!("key: member_id, concurrency: 1{changes}}}"),
    );
    let (dir, project) = project(&report, &ctx);
    (host, ctx, dir, project)
}

#[test]
fn all_or_nothing_rows_deliver_after_every_chunk_has_rendered() {
    let (host, ctx, dir, project) = large_per_row(Script::new(&[]), ", on_row_error: fail");
    let summary = run_once(&ctx, &project, &options());
    assert!(!summary.failed(), "{:?}", summary.outcomes);
    assert_eq!(calls_of(&host, "Write").len(), 300);
    assert_eq!(calls_of(&host, "Deliver").len(), 300);
    let calls = host.calls();
    let last_write = calls.iter().rposition(|c| c.contains(" Write ")).unwrap();
    let first_delivery = calls.iter().position(|c| c.contains(" Deliver ")).unwrap();
    assert!(last_write < first_delivery);
    assert_eq!(
        results_json(&dir)["output_results"][0]["for_each"]["succeeded"],
        300
    );
}

#[test]
fn a_format_failure_in_a_later_chunk_blocks_every_delivery() {
    let (host, ctx, dir, project) = large_per_row(
        Script::new(&[]).fail(Call::Write, 250, "invalid row"),
        ", on_row_error: fail",
    );
    let summary = run_once(&ctx, &project, &options());
    assert!(summary.failed());
    assert_eq!(calls_of(&host, "Write").len(), 300);
    assert!(calls_of(&host, "Deliver").is_empty());
    assert_eq!(results_json(&dir)["output_results"][0]["for_each"]["failed"], 300);
    let starts = calls_of(&host, "Start");
    assert_eq!(calls_of(&host, "Close").len(), starts.len());
}

#[test]
fn filename_collisions_in_later_chunks_fail_before_any_row_is_written() {
    let (host, ctx, dir, _) = large_per_row(Script::new(&[]), "");
    let path = dir.path().join("reports/pack/pack.yml");
    let report = std::fs::read_to_string(&path).unwrap().replace(
        "  format: csv",
        "  format: csv\n  file_name: '{{ each.member_id % 129 }}.csv'",
    );
    std::fs::write(&path, report).unwrap();
    let (project, diags) = crate::project::load_with(dir.path(), &Default::default(), &ctx);
    assert!(!diags.has_errors());
    let summary = run_once(&ctx, &project.unwrap(), &options());
    assert!(summary.failed());
    assert!(
        summary.outcomes[0]
            .error
            .as_ref()
            .unwrap()
            .contains("would both write `1.csv`")
    );
    assert!(calls_of(&host, "Write").is_empty());
    assert!(calls_of(&host, "Deliver").is_empty());
}

#[test]
fn reused_plugins_log_to_the_current_bindings_event_block() {
    let host = host(Script::new(&[]).log_on(Call::Deliver, "sent")).plugin(
        Kind::Format,
        "csv",
        Script::new(&[]).log_on(Call::Write, "written"),
    );
    let ctx = context(&host);
    let report = "queries: [summary]\nsets: [{name: east}, {name: west}]\noutput: {name: detail, format: csv, destination: {profile: outbox, path: 'out/{{ run.set }}/detail.csv'}}\n";
    let (_dir, project) = project(report, &ctx);
    let opts = RunOptions {
        set: Some("all".into()),
        threads: Some(2),
        ..options()
    };
    let (summary, events) = run_recorded(&ctx, &project, &opts);
    assert!(!summary.failed(), "{:?}", summary.outcomes);
    let mut binding = None;
    let mut logs = std::collections::BTreeMap::<String, Vec<String>>::new();
    for event in events {
        match event {
            RunEvent::BindingStart { set, .. } => binding = set,
            RunEvent::PluginLog(_, line) if line == "written" || line == "sent" => {
                logs.entry(binding.clone().expect("a Binding started"))
                    .or_default()
                    .push(line);
            }
            RunEvent::BindingEnd(_) => binding = None,
            _ => {}
        }
    }
    assert_eq!(logs.get("east").unwrap(), &["written", "sent"]);
    assert_eq!(logs.get("west").unwrap(), &["written", "sent"]);
}

/// Members 1, 2, ... with a `dir` each and a `pw` (a secret column).
fn members_in(dirs: &[&str]) -> RecordBatch {
    RecordBatch::try_from_iter([
        (
            "member_id",
            Arc::new(Int64Array::from_iter_values(1..=dirs.len() as i64)) as arrow::array::ArrayRef,
        ),
        (
            "dir",
            Arc::new(StringArray::from_iter_values(dirs.iter().copied())) as arrow::array::ArrayRef,
        ),
        (
            "pw",
            Arc::new(StringArray::from_iter_values(
                (1..=dirs.len()).map(|i| format!("pw-secret-{i}")),
            )) as arrow::array::ArrayRef,
        ),
    ])
    .unwrap()
}

/// A host whose `members` are [`members_in`] `dirs`, with `drop` scripted by `destination`.
fn host_in(dirs: &[&str], destination: Script) -> MemoryHost {
    MemoryHost::new()
        .plugin(
            Kind::Source,
            "mem",
            Script::new(&[dre_protocol::CAP_SESSIONS]).rows("select * from members", members_in(dirs)),
        )
        .plugin(Kind::Format, "csv", Script::new(&[]))
        .plugin(Kind::Destination, "drop", destination)
        .plugin(Kind::Destination, "shelf", Script::new(&[]))
}

/// `--retry-failed` of the Binding's one run so far.
fn retry_of(dir: &TempDir) -> RunOptions {
    let runs = crate::runs::BindingRuns::new(&dir.path().join("target/run/pack/default"));
    let [run_id] = runs.list().try_into().expect("one run");
    retry_run(run_id)
}

fn retry_run(run_id: String) -> RunOptions {
    RunOptions {
        mode: Mode {
            results: Results::Retry(run_id),
            ..Mode::default()
        },
        // Run ids of one second sort by a random suffix: keep the retries too.
        keep_runs: Some(10),
        ..options()
    }
}

const TWO_DESTS: &str = "queries: [members]\noutput:\n  name: letter\n  format: csv\n\
    \x20 for_each: {query: members, key: member_id}\n\
    \x20 destination:\n\
    \x20   - {profile: outbox, path: \"out/{{ each.member_id }}.csv\"}\n\
    \x20   - {profile: archive, path: \"arc/{{ each.member_id }}.csv\"}\n";

#[test]
fn a_retry_resends_only_to_the_destinations_that_failed() {
    let host = host(Script::new(&[]).fail_on(Call::Deliver, "out/101.csv", "the mailbox is full"));
    let ctx = context(&host);
    let (dir, project) = project(TWO_DESTS, &ctx);
    assert!(run_once(&ctx, &project, &options()).failed());
    assert_eq!(calls_of(&host, "Deliver").len(), 4);
    // Row 101 reached the archive; only its outbox delivery failed.
    let retry_host = self::host(Script::new(&[]));
    let second = run_once(&context(&retry_host), &project, &retry_of(&dir));
    assert!(!second.failed(), "{:?}", second.outcomes);
    assert_eq!(
        calls_of(&retry_host, "Deliver"),
        ["destination/drop Deliver out/101.csv"]
    );
}

#[test]
fn a_retry_reads_the_rows_an_earlier_retry_finished_even_without_its_done_file() {
    let host = host(Script::new(&[]).fail_on(Call::Deliver, "out/101.csv", "the mailbox is full"));
    let ctx = context(&host);
    let (dir, project) = project(PER_ROW, &ctx);
    assert!(run_once(&ctx, &project, &options()).failed());
    let runs = crate::runs::BindingRuns::new(&dir.path().join("target/run/pack/default"));
    let [original] = runs.list().try_into().expect("one run");
    let first = self::host(Script::new(&[]));
    assert!(!run_once(&context(&first), &project, &retry_run(original.clone())).failed());
    assert_eq!(
        calls_of(&first, "Deliver"),
        ["destination/drop Deliver out/101.csv"]
    );
    // The first retry stopped before it wrote `done.json`: its rows.jsonl still says 101 went.
    let retried = runs.list().into_iter().find(|r| *r != original).unwrap();
    std::fs::remove_file(runs.run_dir(&retried).join("letter.done.json")).unwrap();
    let again = self::host(Script::new(&[]));
    run_once(&context(&again), &project, &retry_run(original));
    assert!(calls_of(&again, "Deliver").is_empty(), "{:?}", again.calls());
}

#[test]
fn a_row_value_cannot_steer_a_destination_path_out_of_its_folder() {
    let host = host_in(
        &["ok", "../up", "/root", "C:x", "CON", "a\\..\\b"],
        Script::new(&[]),
    );
    let ctx = context(&host);
    let (_dir, project) = project(
        "queries: [members]\noutput:\n  name: letter\n  format: csv\n\
         \x20 for_each: {query: members, key: member_id}\n\
         \x20 destination: {profile: outbox, path: \"{{ each.dir }}/{{ each.member_id }}.csv\"}\n",
        &ctx,
    );
    let (dir, project) = (_dir, project);
    assert!(run_once(&ctx, &project, &options()).failed());
    assert_eq!(calls_of(&host, "Deliver"), ["destination/drop Deliver ok/1.csv"]);
    let s = &results_json(&dir)["output_results"][0]["for_each"];
    assert_eq!((&s["succeeded"], &s["failed"]), (&1.into(), &5.into()), "{s}");
    let keys = s["failed_keys"].as_array().unwrap();
    assert!(
        keys.iter()
            .all(|k| k["message"].as_str().unwrap().contains("refused")),
        "{s}"
    );
}

#[test]
fn a_held_row_whose_path_leaves_its_folder_is_refused_not_held() {
    let host = host_in(&["../up"], Script::new(&[]));
    let ctx = context(&host);
    let (dir, project) = project(
        "queries: [members]\noutput:\n  name: letter\n  format: csv\n\
         \x20 for_each: {query: members, key: member_id}\n\
         \x20 destination: {profile: outbox, path: \"{{ each.dir }}/{{ each.member_id }}.csv\", hold: true}\n",
        &ctx,
    );
    assert!(run_once(&ctx, &project, &options()).failed());
    let runs = crate::runs::BindingRuns::new(&dir.path().join("target/run/pack/default"));
    let run = runs.run_dir(&runs.list().into_iter().last().unwrap());
    let held = std::fs::read_to_string(run.join("held.json")).unwrap_or_default();
    assert!(!held.contains("up"), "{held}");
}

#[test]
fn a_refused_path_with_on_row_error_fail_delivers_no_row() {
    let host = host_in(&["ok", "../up"], Script::new(&[]));
    let ctx = context(&host);
    let (_dir, project) = project(
        "queries: [members]\noutput:\n  name: letter\n  format: csv\n\
         \x20 for_each: {query: members, key: member_id, on_row_error: fail}\n\
         \x20 destination: {profile: outbox, path: \"{{ each.dir }}/{{ each.member_id }}.csv\"}\n",
        &ctx,
    );
    assert!(run_once(&ctx, &project, &options()).failed());
    assert!(calls_of(&host, "Deliver").is_empty());
}

#[test]
fn a_literal_parent_or_absolute_path_in_the_yaml_still_delivers() {
    let host = host_in(&["a", "b"], Script::new(&[]));
    let ctx = context(&host);
    let (_dir, project) = project(
        "queries: [members]\noutput:\n  name: letter\n  format: csv\n\
         \x20 for_each: {query: members, key: member_id}\n\
         \x20 destination: {profile: outbox, path: \"/srv/../out/{{ each.dir }}.csv\"}\n",
        &ctx,
    );
    let summary = run_once(&ctx, &project, &options());
    assert!(!summary.failed(), "{:?}", summary.outcomes);
    assert_eq!(calls_of(&host, "Deliver").len(), 2);
}

#[cfg(windows)]
#[test]
fn a_file_name_with_a_drive_or_a_device_name_is_refused_on_windows() {
    for value in ["C:x", "CON", "nul.txt", "report.csv:hidden"] {
        let host = host_in(&[value], Script::new(&[]));
        let ctx = context(&host);
        let (_dir, project) = project(
            "queries: [members]\noutput:\n  name: letter\n  format: csv\n\
             \x20 file_name: \"{{ each.dir }}\"\n\
             \x20 for_each: {query: members, key: member_id}\n",
            &ctx,
        );
        let summary = run_once(&ctx, &project, &options());
        assert!(summary.failed(), "{value}");
        assert!(calls_of(&host, "Write").is_empty(), "{value}");
    }
}

#[test]
fn rows_rendering_the_same_path_fail_the_second_instead_of_overwriting() {
    let host = host_in(&["x", "x", "y"], Script::new(&[]));
    let ctx = context(&host);
    let (_dir, project) = project(
        "queries: [members]\noutput:\n  name: letter\n  format: csv\n\
         \x20 for_each: {query: members, key: member_id}\n\
         \x20 destination: {profile: outbox, path: \"out/{{ each.dir }}.csv\"}\n",
        &ctx,
    );
    let (dir, project) = (_dir, project);
    assert!(run_once(&ctx, &project, &options()).failed());
    assert_eq!(
        calls_of(&host, "Deliver"),
        [
            "destination/drop Deliver out/x.csv",
            "destination/drop Deliver out/y.csv"
        ]
    );
    let s = &results_json(&dir)["output_results"][0]["for_each"];
    assert_eq!(s["failed"], 1, "{s}");
    let message = s["failed_keys"][0]["message"].as_str().unwrap();
    assert!(message.contains("delivers to `out/x.csv`"), "{s}");
}

#[test]
fn sets_running_at_once_cannot_deliver_rows_to_the_same_path() {
    let host = host_in(&["x"], Script::new(&[]));
    let ctx = context(&host);
    let (_dir, project) = project(
        "queries: [members]\nsets: [{name: east}, {name: west}]\noutput:\n  name: letter\n  format: csv\n\
         \x20 for_each: {query: members, key: member_id}\n\
         \x20 destination: {profile: outbox, path: \"out/{{ each.dir }}.csv\"}\n",
        &ctx,
    );
    let opts = RunOptions {
        set: Some("all".into()),
        threads: Some(2),
        ..options()
    };
    let summary = run_once(&ctx, &project, &opts);
    assert!(summary.failed());
    assert_eq!(calls_of(&host, "Deliver"), ["destination/drop Deliver out/x.csv"]);
}

#[test]
fn per_row_saved_results_mask_secret_columns_and_a_retry_runs_the_queries_again() {
    let host = host_in(
        &["a", "b"],
        Script::new(&[]).fail_on(Call::Deliver, "out/1.csv", "full"),
    );
    let ctx = context(&host);
    let (dir, project) = project(
        "queries:\n  - {query: members, secret_columns: [pw]}\noutput:\n  name: letter\n  format: csv\n\
         \x20 for_each: {query: members, key: member_id}\n\
         \x20 destination: {profile: outbox, path: \"out/{{ each.member_id }}.csv\"}\n",
        &ctx,
    );
    assert!(run_once(&ctx, &project, &options()).failed());
    let runs = crate::runs::BindingRuns::new(&dir.path().join("target/run/pack/default"));
    let results = runs
        .run_dir(&runs.list().into_iter().next().unwrap())
        .join("results");
    for f in std::fs::read_dir(&results).unwrap() {
        let bytes = std::fs::read(f.unwrap().path()).unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains("pw-secret"));
    }
    let again = host_in(&["a", "b"], Script::new(&[]));
    let second = run_once(&context(&again), &project, &retry_of(&dir));
    assert!(!second.failed(), "{:?}", second.outcomes);
    // The saved `pw` is `*****`: the retry read the members again instead.
    assert_eq!(calls_of(&again, "Execute").len(), 1, "{:?}", again.calls());
    assert_eq!(
        calls_of(&again, "Deliver"),
        ["destination/drop Deliver out/1.csv"]
    );
}

#[test]
fn a_release_record_masks_the_secrets_a_destination_error_echoes() {
    let host = host(Script::new(&[]).fail_on(Call::Deliver, "out/detail.csv", "denied for tok-s3cret-value"));
    let ctx = Context::new(
        Env::from_vars([("DRE_TEST_TOKEN", "tok-s3cret-value")]),
        Arc::new(host.clone()),
    );
    let (dir, project) = project_with_profiles(
        "queries: [summary]\noutput:\n  name: detail\n  format: csv\n\
         \x20 destination: {profile: outbox, path: out/detail.csv, hold: true}\n",
        &PROFILES.replace(
            "dev: {type: drop}",
            "dev: {type: drop, token: \"{{ env_var('DRE_TEST_TOKEN') }}\"}",
        ),
        &ctx,
    );
    assert!(!run_once(&ctx, &project, &options()).failed());
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
    assert!(released.failed());
    let record =
        std::fs::read_to_string(runs.run_dir(&released.release_id).join("run_results.json")).unwrap();
    assert!(record.contains("denied for"), "{record}");
    assert!(!record.contains("tok-s3cret-value"), "{record}");
}

/// A source that asks for backslash string literals (as ClickHouse does) gets `O'Brien` from a
/// lookup as `'O\'Brien'`, and the statements around it still split where they should.
#[test]
fn a_lookup_quote_in_backslash_literals_keeps_the_statements_apart() {
    let description = dre_protocol::host::Description {
        string_literal: Some("backslash".into()),
        ..Default::default()
    };
    let host = MemoryHost::new()
        .plugin(
            Kind::Source,
            "mem",
            Script::new(&[dre_protocol::CAP_SESSIONS])
                .rows("select * from x", summary())
                .description(description),
        )
        .plugin(Kind::Format, "csv", Script::new(&[]))
        .plugin(Kind::Destination, "drop", Script::new(&[]));
    let ctx = context(&host);
    let (dir, _) = project(ONE_FILE, &ctx);
    std::fs::create_dir_all(dir.path().join("lookups")).unwrap();
    std::fs::write(dir.path().join("lookups/owners.csv"), "name\nO'Brien\n").unwrap();
    std::fs::write(
        dir.path().join("reports/pack/summary.sql"),
        "create temporary table x as select * from {{ ref('owners') }}; select * from x",
    )
    .unwrap();
    let project = crate::project::load_with(dir.path(), &Default::default(), &ctx)
        .0
        .unwrap();
    let summary = run_once(&ctx, &project, &options());
    assert_eq!(
        summary.outcomes[0].status,
        Status::Success,
        "{:?}",
        summary.outcomes
    );
    let executed = calls_of(&host, "Execute");
    assert_eq!(executed.len(), 2, "{executed:?}");
    assert!(executed.iter().any(|c| c.contains(r"'O\'Brien'")), "{executed:?}");
}

#[test]
fn per_row_email_refuses_recipient_values_containing_commas() {
    for recipient in ["to", "cc", "bcc"] {
        for (kind, value, address, fails) in [
            (
                "email",
                "\"{{ each.name }}\"",
                "ann@example.com, outsider@example.com",
                true,
            ),
            (
                "email",
                "[\"{{ each.name }}\"]",
                "ann@example.com, outsider@example.com",
                true,
            ),
            ("email", "\"{{ each.name }}\"", "ann@example.com", false),
            (
                "email",
                "[\"{{ each.name }}\", boss@example.com]",
                "ann@example.com",
                false,
            ),
            ("email", "\"ann@example.com, boss@example.com\"", "ignored", false),
            (
                "drop",
                "\"{{ each.name }}\"",
                "ann@example.com, outsider@example.com",
                false,
            ),
        ] {
            let rows = RecordBatch::try_from_iter([
                (
                    "member_id",
                    Arc::new(Int64Array::from(vec![101])) as arrow::array::ArrayRef,
                ),
                (
                    "name",
                    Arc::new(StringArray::from(vec![address])) as arrow::array::ArrayRef,
                ),
            ])
            .unwrap();
            let host = host(Script::new(&[]))
                .plugin(
                    Kind::Source,
                    "mem",
                    Script::new(&[dre_protocol::CAP_SESSIONS]).rows("select * from members", rows),
                )
                .plugin(Kind::Destination, "email", Script::new(&[]));
            let ctx = context(&host);
            let report = format!(
                "queries: [members]\noutput:\n  name: letter\n  format: csv\n  for_each: {{query: members, key: member_id}}\n  destination: {{profile: outbox, {recipient}: {value}}}\n"
            );
            let (_dir, project) = project_with_profiles(
                &report,
                &PROFILES.replace("type: drop", &format!("type: {kind}")),
                &ctx,
            );
            let result = run_once(&ctx, &project, &options());
            assert_eq!(
                result.failed(),
                fails,
                "{recipient} {kind} {value}: {:?}",
                result.outcomes
            );
            assert_eq!(
                calls_of(&host, "Deliver").len(),
                usize::from(!fails),
                "{:?}",
                host.calls()
            );
            if fails {
                assert!(
                    result.outcomes[0]
                        .error
                        .as_deref()
                        .unwrap()
                        .contains("must not contain a comma")
                );
            }
        }
    }
}
