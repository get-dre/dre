//! Delivery in-process, through scripted plugins from [`MemoryHost`].

use std::sync::Arc;

use dre_protocol::Kind;
use tempfile::TempDir;

use super::*;
use crate::Env;
use crate::plugin_host::memory::{Call, MemoryHost, Script};

const PROFILES: &str = "connections:\n  warehouse:\n    targets:\n      dev: {type: mem}\n\
destinations:\n\
\x20 outbox:\n    targets:\n      dev: {type: drop}\n\
\x20 bulk:\n    targets:\n      dev: {type: bulk}\n\
\x20 chat:\n    targets:\n      dev: {type: chat}\n\
\x20 pager:\n    targets:\n      dev: {type: pager}\n\
\x20 disk:\n    targets:\n      dev: {type: local}\n\
\x20 off:\n    targets:\n      dev: {deliver: false}\n";

fn host(drop: Script) -> MemoryHost {
    MemoryHost::new()
        .plugin(Kind::Source, "mem", Script::new(&[]))
        .plugin(Kind::Destination, "drop", drop)
        .plugin(
            Kind::Destination,
            "bulk",
            Script::new(&[dre_protocol::CAP_MULTI_FILE]),
        )
        .plugin(
            Kind::Destination,
            "chat",
            Script::new(&[dre_protocol::CAP_MESSAGE]),
        )
        .plugin(
            Kind::Destination,
            "pager",
            Script::new(&[dre_protocol::CAP_MESSAGE_ONLY]),
        )
}

/// A project with the destinations above and two files in its target path, `a.csv` and `b.csv`.
struct Fixture {
    dir: TempDir,
    project: Project,
    ctx: Context,
    host: MemoryHost,
}

impl Fixture {
    fn new(drop: Script) -> Fixture {
        let host = host(drop);
        let ctx = Context::new(Env::default(), Arc::new(host.clone()));
        let dir = tempfile::tempdir().unwrap();
        for (path, text) in [
            ("dre_project.yml", "name: d\ndefault_profile: warehouse\n"),
            ("profiles.yml", PROFILES),
            ("target/run/a.csv", "a\n"),
            ("target/run/b.csv", "b\n"),
            ("target/run/daily.md", "# Daily\n"),
        ] {
            let p = dir.path().join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
        let (project, diags) = crate::project::load_with(dir.path(), &Default::default(), &ctx);
        assert!(!diags.has_errors(), "{:?}", diags.sorted());
        Fixture {
            dir,
            project: project.unwrap(),
            ctx,
            host,
        }
    }

    fn file(&self, name: &str) -> PathBuf {
        self.dir.path().join("target/run").join(name)
    }

    fn deliverer(&self) -> Deliverer<'_> {
        Deliverer::new(&self.ctx, &self.project, Arc::new(|_, _| {}))
    }

    /// The calls of `kind`, without the plugin's name.
    fn calls(&self, call: &str) -> Vec<String> {
        self.host
            .calls()
            .into_iter()
            .filter(|c| c.split_whitespace().nth(1) == Some(call))
            .collect()
    }
}

fn no_options() -> JsonMap<String, Json> {
    JsonMap::new()
}

#[test]
fn each_file_goes_to_the_folder_of_the_entry_path() {
    let f = Fixture::new(Script::new(&[]));
    let files = [f.file("a.csv"), f.file("b.csv")];
    let options = no_options();
    let sent = f
        .deliverer()
        .deliver(Request::new("outbox", Some("out/detail.csv"), &options, &files))
        .unwrap()
        .unwrap();
    assert_eq!(
        f.calls("Deliver"),
        [
            "destination/drop Deliver out/a.csv",
            "destination/drop Deliver out/b.csv"
        ]
    );
    assert_eq!(sent.location, "memory://out/a.csv, memory://out/b.csv");
    assert_eq!(
        sent.files,
        [
            Some("memory://out/a.csv".into()),
            Some("memory://out/b.csv".into())
        ]
    );
}

#[test]
fn a_multi_file_destination_gets_every_file_in_one_send() {
    let f = Fixture::new(Script::new(&[]));
    let files = [f.file("a.csv"), f.file("b.csv")];
    let options = no_options();
    let sent = f
        .deliverer()
        .deliver(Request::new("bulk", Some("out/"), &options, &files))
        .unwrap()
        .unwrap();
    assert_eq!(
        f.calls("Deliver"),
        ["destination/bulk Deliver out/a.csv out/b.csv"]
    );
    assert_eq!(sent.files[0], sent.files[1]);
}

#[test]
fn a_message_goes_as_a_message_where_the_destination_takes_one_else_as_its_file() {
    let f = Fixture::new(Script::new(&[]));
    let files = [f.file("daily.md")];
    let attach = [f.file("a.csv")];
    let message = ("Daily".to_string(), "Revenue 12".to_string());
    let options = no_options();
    let mut d = f.deliverer();
    let to_chat = Request::new("chat", None, &options, &files).message(Some(&message), &attach);
    let sent = d.deliver(to_chat).unwrap().unwrap();
    let to_outbox = Request::new("outbox", Some("daily.md"), &options, &files).message(Some(&message), &[]);
    d.deliver(to_outbox).unwrap().unwrap();
    let attached = f.file("a.csv").to_string_lossy().into_owned();
    assert_eq!(
        f.calls("Deliver"),
        [
            format!("destination/chat Deliver message `Daily` {attached}"),
            "destination/drop Deliver daily.md".to_string(),
        ]
    );
    assert_eq!(sent.files, [Some(format!("memory://{attached}"))]);
}

#[test]
fn a_message_only_destination_refuses_files() {
    let f = Fixture::new(Script::new(&[]));
    let files = [f.file("a.csv")];
    let options = no_options();
    let e = f
        .deliverer()
        .deliver(Request::new("pager", None, &options, &files).format("csv"))
        .unwrap_err();
    assert_eq!(e.code, Code::DeliveryFailed);
    assert!(
        e.message
            .starts_with("`pager` only takes messages, but this output is `csv`"),
        "{e}"
    );
    assert!(f.calls("Deliver").is_empty());
}

#[test]
fn a_local_destination_copies_into_the_project_by_its_rules() {
    let f = Fixture::new(Script::new(&[]));
    let files = [f.file("a.csv")];
    let mut options = no_options();
    options.insert("if_exists".into(), Json::from("error"));
    let mut d = f.deliverer();
    let sent = d
        .deliver(Request::new("disk", Some("exports/a.csv"), &options, &files))
        .unwrap()
        .unwrap();
    let copied = f.dir.path().join("exports/a.csv");
    assert_eq!(std::fs::read_to_string(&copied).unwrap(), "a\n");
    assert_eq!(sent.files, [Some(copied.to_string_lossy().into_owned())]);
    // The second time, the file is there: `if_exists: error`.
    let e = d
        .deliver(Request::new("disk", Some("exports/a.csv"), &options, &files))
        .unwrap_err();
    assert_eq!(e.code.as_str(), "local/file-exists");
    assert!(e.message.ends_with("; the output is still in target/"), "{e}");
    assert!(f.host.calls().is_empty(), "no plugin for a local destination");
}

#[test]
fn an_entry_that_delivers_nowhere_sends_nothing() {
    let f = Fixture::new(Script::new(&[]));
    let files = [f.file("a.csv")];
    let options = no_options();
    let sent = f
        .deliverer()
        .deliver(Request::new("off", Some("x.csv"), &options, &files))
        .unwrap();
    assert_eq!(sent, None);
    assert!(f.host.calls().is_empty());
}

#[test]
fn a_destination_process_is_reused_until_a_delivery_through_it_fails() {
    let f = Fixture::new(Script::new(&[]).fail(Call::Deliver, 2, "the mailbox is full"));
    let files = [f.file("a.csv")];
    let options = no_options();
    let mut d = f.deliverer();
    let send = |d: &mut Deliverer<'_>| d.deliver(Request::new("outbox", Some("a.csv"), &options, &files));
    send(&mut d).unwrap();
    let e = send(&mut d).unwrap_err();
    assert_eq!(e.code, Code::DeliveryFailed);
    assert!(e.message.starts_with("delivery through `drop` failed: "), "{e}");
    assert!(e.message.contains("the mailbox is full"), "{e}");
    assert!(e.message.ends_with("; the output is still in target/"), "{e}");
    // The retry goes through a fresh process.
    send(&mut d).unwrap();
    assert_eq!(f.calls("Start").len(), 2);
    assert_eq!(f.calls("Close").len(), 1);
    drop(d);
    assert_eq!(f.calls("Close").len(), 2);
}

#[test]
fn a_refused_claim_sends_nothing() {
    let f = Fixture::new(Script::new(&[]));
    let files = [f.file("a.csv")];
    let options = no_options();
    let taken = |remote: &str| -> Result<(), Fail> { Err(format!("`{remote}` is taken").into()) };
    let e = f
        .deliverer()
        .deliver(Request::new("outbox", Some("a.csv"), &options, &files).claim(&taken))
        .unwrap_err();
    assert_eq!(e.message, "`a.csv` is taken");
    assert!(f.calls("Deliver").is_empty());
}

#[test]
fn remote_paths_follow_the_entry_path() {
    let one = [PathBuf::from("/t/a.csv")];
    let two = [PathBuf::from("/t/a.csv"), PathBuf::from("/t/b.csv")];
    let remote = |path: Option<&str>, files: &[PathBuf]| -> Vec<Option<String>> {
        targets(path, files).into_iter().map(|t| t.remote_path).collect()
    };
    type Case<'a> = (Option<&'a str>, &'a [PathBuf], Vec<Option<&'a str>>);
    let cases: [Case<'_>; 5] = [
        (Some("out/x.csv"), &one, vec![Some("out/x.csv")]),
        (
            Some("out/x.csv"),
            &two,
            vec![Some("out/a.csv"), Some("out/b.csv")],
        ),
        (Some("x.csv"), &two, vec![Some("a.csv"), Some("b.csv")]),
        (
            Some("in\\x.csv"),
            &two,
            vec![Some("in\\a.csv"), Some("in\\b.csv")],
        ),
        (None, &two, vec![None, None]),
    ];
    for (path, files, want) in cases {
        let want: Vec<Option<String>> = want.into_iter().map(|w| w.map(str::to_string)).collect();
        assert_eq!(remote(path, files), want, "{path:?}");
    }
}
