//! Slack delivery against a local fake of the Web API endpoints the plugin uses (there's no
//! official emulator). Against a real workspace when `DRE_TEST_SLACK_TOKEN` and
//! `DRE_TEST_SLACK_CHANNEL` are set (and a DM with `DRE_TEST_SLACK_USER`): a manual smoke test,
//! never run in CI.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::{Arc, Mutex};

use dre_plugin_host::conformance;
use dre_plugin_host::host::{LogSink, PluginProcess};
use dre_protocol::msg::DeliveryFile;
use dre_protocol::msg::Message;
use dre_protocol::{CAP_MESSAGE, CAP_MULTI_FILE};
use serde_json::{Map, Value, json};

fn bin() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_dre-plugin-slack"))
}

/// One request the fake received: path, form fields (or raw body length for uploads), token.
#[derive(Debug, Clone)]
struct Call {
    path: String,
    form: BTreeMap<String, String>,
    body_len: usize,
    token: String,
}

type Calls = Arc<Mutex<Vec<Call>>>;

/// Tokens change the fake's behaviour: `bad` → invalid_auth everywhere, `noscope` → missing_scope
/// on conversations.list, `slow` → the first upload-URL call is rate limited, `busy` → every
/// upload-URL call is, `nochat` → missing_scope on chat.postMessage. Channel `C_OUT` →
/// not_in_channel on completion; user `U_OFF` has the app's Messages tab off.
fn fake_slack() -> (String, Calls) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let calls: Calls = Arc::default();
    let (c, b) = (calls.clone(), base.clone());
    std::thread::spawn(move || {
        let mut next_file = 0;
        let mut slow_once = true;
        for stream in listener.incoming().flatten() {
            let mut r = BufReader::new(stream.try_clone().unwrap());
            let mut s = stream;
            let mut line = String::new();
            if r.read_line(&mut line).unwrap_or(0) == 0 {
                continue;
            }
            let path = line.split_whitespace().nth(1).unwrap().to_string();
            let (mut len, mut token) = (0usize, String::new());
            loop {
                let mut h = String::new();
                r.read_line(&mut h).unwrap();
                let h = h.trim_end().to_string();
                if h.is_empty() {
                    break;
                }
                let (k, v) = h.split_once(':').unwrap();
                match k.to_ascii_lowercase().as_str() {
                    "content-length" => len = v.trim().parse().unwrap(),
                    "authorization" => token = v.trim().trim_start_matches("Bearer ").to_string(),
                    _ => {}
                }
            }
            let mut body = vec![0; len];
            r.read_exact(&mut body).unwrap();
            let form: BTreeMap<String, String> = if path.starts_with("/api/") {
                form_urlencoded::parse(&body).into_owned().collect()
            } else {
                BTreeMap::new()
            };
            c.lock().unwrap().push(Call {
                path: path.clone(),
                form: form.clone(),
                body_len: len,
                token: token.clone(),
            });
            let method = path.trim_start_matches("/api/");
            let (status, extra, reply) = if path.starts_with("/upload/") {
                ("200 OK", "", "OK".to_string())
            } else if token == "bad" {
                (
                    "200 OK",
                    "",
                    json!({"ok": false, "error": "invalid_auth"}).to_string(),
                )
            } else if method == "files.getUploadURLExternal"
                && (token == "busy" || (token == "slow" && std::mem::take(&mut slow_once)))
            {
                ("429 Too Many Requests", "Retry-After: 1\r\n", String::new())
            } else {
                let v = match method {
                    "files.getUploadURLExternal" => {
                        next_file += 1;
                        json!({"ok": true, "upload_url": format!("{b}/upload/F{next_file}"), "file_id": format!("F{next_file}")})
                    }
                    "files.completeUploadExternal" if form["channel_id"] == "C_OUT" => {
                        json!({"ok": false, "error": "not_in_channel"})
                    }
                    "files.completeUploadExternal" => {
                        let files: Vec<Value> = serde_json::from_str(&form["files"]).unwrap();
                        let out: Vec<Value> = files
                            .iter()
                            .map(|f| json!({"id": f["id"], "title": f["title"], "permalink": format!("https://acme.slack.com/files/{}", f["id"].as_str().unwrap())}))
                            .collect();
                        json!({"ok": true, "files": out})
                    }
                    "conversations.list" if token == "noscope" => {
                        json!({"ok": false, "error": "missing_scope", "needed": "channels:read"})
                    }
                    "conversations.list" if !form.contains_key("cursor") => json!({
                        "ok": true, "channels": [{"id": "C1", "name": "general"}],
                        "response_metadata": {"next_cursor": "page2"}}),
                    "conversations.list" => json!({
                        "ok": true, "channels": [{"id": "C2", "name": "finance-reports"}],
                        "response_metadata": {"next_cursor": ""}}),
                    "chat.postMessage" if token == "nochat" => {
                        json!({"ok": false, "error": "missing_scope", "needed": "chat:write"})
                    }
                    "chat.postMessage" if form["channel"] == "D_U_OFF" => {
                        json!({"ok": false, "error": "messages_tab_disabled"})
                    }
                    // The plugin only ever probes a DM with no text.
                    "chat.postMessage" if !form.contains_key("text") => {
                        json!({"ok": false, "error": "no_text"})
                    }
                    "chat.postMessage" if form["channel"] == "C_BADBLOCKS" => {
                        json!({"ok": false, "error": "invalid_blocks", "response_metadata": {"messages": ["[ERROR] bad block"]}})
                    }
                    "chat.getPermalink" => {
                        json!({"ok": true, "permalink": format!("https://acme.slack.com/archives/{}/p{}", form["channel"], form["message_ts"].replace('.', ""))})
                    }
                    "chat.postMessage" => {
                        json!({"ok": true, "channel": form["channel"], "ts": "1700000000.000100"})
                    }
                    "conversations.open" => {
                        json!({"ok": true, "channel": {"id": format!("D_{}", form["users"])}})
                    }
                    _ => json!({"ok": false, "error": "unknown_method"}),
                };
                ("200 OK", "", v.to_string())
            };
            let _ = write!(
                s,
                "HTTP/1.1 {status}\r\n{extra}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                reply.len()
            );
        }
    });
    (base, calls)
}

fn quiet() -> LogSink {
    Arc::new(|_, _| {})
}

type Logs = Arc<Mutex<Vec<String>>>;

fn logging() -> (LogSink, Logs) {
    let logs: Logs = Arc::default();
    let l = logs.clone();
    (Arc::new(move |_, m| l.lock().unwrap().push(m.to_string())), logs)
}

fn obj(v: Value) -> Map<String, Value> {
    v.as_object().unwrap().clone()
}

struct Files(tempfile::TempDir);

impl Files {
    fn new() -> Files {
        Files(tempfile::tempdir().unwrap())
    }
    fn file(&self, name: &str, bytes: &[u8]) -> DeliveryFile {
        let p = self.0.path().join(name);
        std::fs::write(&p, bytes).unwrap();
        DeliveryFile {
            local_path: p.to_string_lossy().to_string(),
            remote_path: None,
        }
    }
}

fn send(files: &[DeliveryFile], conn: Value, options: Value) -> Result<String, String> {
    let mut p = PluginProcess::start(bin(), quiet()).unwrap();
    let r = p
        .deliver_files(files, obj(conn), obj(options))
        .map_err(|e| e.to_string());
    let _ = p.close();
    r
}

fn conn(base: &str, token: &str) -> Value {
    json!({"token": token, "api_url": format!("{base}/api")})
}

fn calls_to(calls: &Calls, method: &str) -> Vec<Call> {
    calls
        .lock()
        .unwrap()
        .iter()
        .filter(|c| c.path == format!("/api/{method}"))
        .cloned()
        .collect()
}

#[test]
fn conforms_to_the_protocol() {
    conformance::assert_conforms(bin());
}

#[test]
fn advertises_multi_file() {
    let p = PluginProcess::start(bin(), quiet()).unwrap();
    assert!(p.has(CAP_MULTI_FILE));
}

#[test]
fn uploads_to_a_channel_id_with_the_message() {
    let (base, calls) = fake_slack();
    let f = Files::new();
    let loc = send(
        &[f.file("daily.csv", b"n\r\n1\r\n")],
        conn(&base, "xoxb-good"),
        json!({"channel": "C9", "message": "Daily report for client_a"}),
    )
    .unwrap();
    assert_eq!(loc, "slack C9: https://acme.slack.com/files/F1");

    let get = calls_to(&calls, "files.getUploadURLExternal");
    assert_eq!(get.len(), 1);
    assert_eq!(get[0].form["filename"], "daily.csv");
    assert_eq!(get[0].form["length"], "6");
    assert_eq!(get[0].token, "xoxb-good");
    let upload: Vec<Call> = calls
        .lock()
        .unwrap()
        .iter()
        .filter(|c| c.path == "/upload/F1")
        .cloned()
        .collect();
    assert_eq!(upload[0].body_len, 6);
    let done = calls_to(&calls, "files.completeUploadExternal");
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].form["channel_id"], "C9");
    assert_eq!(done[0].form["initial_comment"], "Daily report for client_a");
    let files: Value = serde_json::from_str(&done[0].form["files"]).unwrap();
    assert_eq!(files, json!([{"id": "F1", "title": "daily.csv"}]));
}

#[test]
fn several_files_land_in_one_post() {
    let (base, calls) = fake_slack();
    let f = Files::new();
    let loc = send(
        &[
            f.file("daily_Orders.csv", b"o"),
            f.file("daily_Refunds.csv", b"r"),
        ],
        conn(&base, "xoxb-good"),
        json!({"channel": "C9"}),
    )
    .unwrap();
    assert!(
        loc.ends_with("files/F1, https://acme.slack.com/files/F2"),
        "{loc}"
    );
    assert_eq!(calls_to(&calls, "files.getUploadURLExternal").len(), 2);
    let done = calls_to(&calls, "files.completeUploadExternal");
    assert_eq!(done.len(), 1);
    let files: Vec<Value> = serde_json::from_str(&done[0].form["files"]).unwrap();
    assert_eq!(files.len(), 2);
    assert!(!done[0].form.contains_key("initial_comment"));
}

#[test]
fn a_channel_name_is_resolved_across_pages() {
    let (base, calls) = fake_slack();
    let f = Files::new();
    let loc = send(
        &[f.file("a.csv", b"1")],
        conn(&base, "xoxb-good"),
        json!({"channel": "#finance-reports"}),
    )
    .unwrap();
    assert!(loc.starts_with("slack #finance-reports (C2)"), "{loc}");
    assert_eq!(calls_to(&calls, "conversations.list").len(), 2);
    assert_eq!(
        calls_to(&calls, "files.completeUploadExternal")[0].form["channel_id"],
        "C2"
    );

    let err = send(
        &[f.file("a.csv", b"1")],
        conn(&base, "xoxb-good"),
        json!({"channel": "#nope"}),
    )
    .unwrap_err();
    assert!(err.contains("no Slack channel named #nope"), "{err}");
}

#[test]
fn a_user_gets_the_file_in_a_dm() {
    let (base, calls) = fake_slack();
    let f = Files::new();
    let loc = send(
        &[f.file("a.csv", b"1")],
        conn(&base, "xoxb-good"),
        json!({"user": "U42"}),
    )
    .unwrap();
    assert!(loc.starts_with("slack DM with U42"), "{loc}");
    assert_eq!(calls_to(&calls, "conversations.open")[0].form["users"], "U42");
    let probe = calls_to(&calls, "chat.postMessage");
    assert_eq!(probe.len(), 1);
    assert_eq!(probe[0].form["channel"], "D_U42");
    assert!(
        !probe[0].form.contains_key("text"),
        "the probe must not post anything"
    );
    assert_eq!(
        calls_to(&calls, "files.completeUploadExternal")[0].form["channel_id"],
        "D_U42"
    );
}

#[test]
fn a_dm_with_the_messages_tab_off_fails_before_uploading() {
    let (base, calls) = fake_slack();
    let f = Files::new();
    let err = send(
        &[f.file("a.csv", b"1")],
        conn(&base, "xoxb-good"),
        json!({"user": "U_OFF"}),
    )
    .unwrap_err();
    assert!(err.contains("turn on App Home > Messages Tab"), "{err}");
    assert!(calls_to(&calls, "files.getUploadURLExternal").is_empty());
}

#[test]
fn a_dm_needs_chat_write() {
    let (base, calls) = fake_slack();
    let f = Files::new();
    let err = send(
        &[f.file("a.csv", b"1")],
        conn(&base, "nochat"),
        json!({"user": "U42"}),
    )
    .unwrap_err();
    assert!(err.contains("lacks the `chat:write` scope"), "{err}");
    assert!(calls_to(&calls, "files.getUploadURLExternal").is_empty());
}

#[test]
fn a_channel_is_not_probed() {
    let (base, calls) = fake_slack();
    let f = Files::new();
    send(
        &[f.file("a.csv", b"1")],
        conn(&base, "xoxb-good"),
        json!({"channel": "C7"}),
    )
    .unwrap();
    assert!(calls_to(&calls, "chat.postMessage").is_empty());
}

#[test]
fn the_profile_channel_is_the_default() {
    let (base, calls) = fake_slack();
    let f = Files::new();
    let mut c = conn(&base, "xoxb-good");
    c["channel"] = json!("C7");
    send(&[f.file("a.csv", b"1")], c, json!({"message": "hi"})).unwrap();
    assert_eq!(
        calls_to(&calls, "files.completeUploadExternal")[0].form["channel_id"],
        "C7"
    );
}

#[test]
fn an_empty_channel_or_user_never_falls_back_to_the_profile_channel() {
    // A per-row `channel: "{{ each.channel }}"` whose row has no channel renders empty: that
    // row's file must not reach the profile's default channel.
    let (base, calls) = fake_slack();
    let f = Files::new();
    let mut c = conn(&base, "xoxb-good");
    c["channel"] = json!("C7");
    for options in [
        json!({"channel": ""}),
        json!({"channel": "  "}),
        json!({"user": ""}),
    ] {
        let err = send(&[f.file("a.csv", b"1")], c.clone(), options).unwrap_err();
        assert!(err.contains("is empty"), "{err}");
    }
    assert!(calls.lock().unwrap().is_empty());
}

#[test]
fn target_mistakes_fail_before_any_api_call() {
    let (base, calls) = fake_slack();
    let f = Files::new();
    let err = send(
        &[f.file("a.csv", b"1")],
        conn(&base, "xoxb-good"),
        json!({"channel": "C1", "user": "U1"}),
    )
    .unwrap_err();
    assert!(err.contains("either `channel` or `user`, not both"), "{err}");
    let err = send(&[f.file("a.csv", b"1")], conn(&base, "xoxb-good"), json!({})).unwrap_err();
    assert!(err.contains("needs `channel:`"), "{err}");
    let err = send(
        &[f.file("a.csv", b"1")],
        conn(&base, "xoxb-good"),
        json!({"chanel": "C1"}),
    )
    .unwrap_err();
    assert!(
        err.contains("unknown option `chanel` for destination `slack`"),
        "{err}"
    );
    assert!(calls.lock().unwrap().is_empty());
}

#[test]
fn slack_errors_become_clear_messages_without_the_token() {
    let (base, _calls) = fake_slack();
    let f = Files::new();
    let err = send(
        &[f.file("a.csv", b"1")],
        conn(&base, "bad"),
        json!({"channel": "C9"}),
    )
    .unwrap_err();
    assert!(
        err.contains("Slack rejected the bot token (invalid_auth)"),
        "{err}"
    );
    let err = send(
        &[f.file("a.csv", b"1")],
        conn(&base, "xoxb-secret-value"),
        json!({"channel": "C_OUT"}),
    )
    .unwrap_err();
    assert!(err.contains("the bot isn't a member of channel C_OUT"), "{err}");
    assert!(!err.contains("xoxb-secret-value"), "{err}");
    let err = send(
        &[f.file("a.csv", b"1")],
        conn(&base, "noscope"),
        json!({"channel": "#general"}),
    )
    .unwrap_err();
    assert!(err.contains("lacks the `channels:read` scope"), "{err}");
}

#[test]
fn a_rate_limited_call_is_tried_again() {
    let (base, calls) = fake_slack();
    let f = Files::new();
    send(
        &[f.file("a.csv", b"1")],
        conn(&base, "slow"),
        json!({"channel": "C9"}),
    )
    .unwrap();
    assert_eq!(calls_to(&calls, "files.getUploadURLExternal").len(), 2);

    let err = send(
        &[f.file("a.csv", b"1")],
        conn(&base, "busy"),
        json!({"channel": "C9"}),
    )
    .unwrap_err();
    assert!(err.contains("rate-limiting files.getUploadURLExternal"), "{err}");
}

#[test]
fn delivers_to_a_real_workspace() {
    let (Ok(token), Ok(channel)) = (
        std::env::var("DRE_TEST_SLACK_TOKEN"),
        std::env::var("DRE_TEST_SLACK_CHANNEL"),
    ) else {
        eprintln!("skipped: set DRE_TEST_SLACK_TOKEN and DRE_TEST_SLACK_CHANNEL for a manual smoke test");
        return;
    };
    let f = Files::new();
    let loc = send(
        &[f.file("dre-smoke.csv", b"n\r\n1\r\n")],
        json!({"token": token}),
        json!({"channel": channel, "message": "DRE Slack destination smoke test"}),
    )
    .unwrap();
    assert!(loc.starts_with("slack "), "{loc}");
    if let Ok(user) = std::env::var("DRE_TEST_SLACK_USER") {
        let loc = send(
            &[f.file("dre-smoke-dm.csv", b"n\r\n1\r\n")],
            json!({"token": std::env::var("DRE_TEST_SLACK_TOKEN").unwrap()}),
            json!({"user": user, "message": "DRE Slack DM smoke test"}),
        )
        .unwrap();
        assert!(loc.starts_with("slack DM with "), "{loc}");
    }
}

fn post(m: &Message, attach: &[DeliveryFile], conn: Value, options: Value) -> Result<String, String> {
    let mut p = PluginProcess::start(bin(), quiet()).unwrap();
    let r = p
        .deliver_message(m, attach, obj(conn), obj(options))
        .map_err(|e| e.to_string());
    let _ = p.close();
    r
}

fn message(f: &Files, title: &str, text: &str) -> Message {
    let md = f.file("daily.md", format!("# {title}\n\n{text}\n").as_bytes());
    Message {
        title: title.into(),
        text: text.into(),
        html: None,
        path: md.local_path,
    }
}

#[test]
fn advertises_messages_and_their_limit() {
    let mut p = PluginProcess::start(bin(), quiet()).unwrap();
    assert!(p.has(CAP_MESSAGE));
    assert_eq!(p.description().unwrap().message_limit, Some(4_000));
}

#[test]
fn posts_a_message_as_mrkdwn_with_values_escaped() {
    let (base, calls) = fake_slack();
    let f = Files::new();
    let m = message(
        &f,
        "Daily *revenue*",
        "Revenue **€12,340** (_+4.1%_)\n- top: acme\\_corp 2\\*3 <b>\n- [Report](https://x.test/r_1)",
    );
    let loc = post(
        &m,
        &[],
        conn(&base, "xoxb-good"),
        json!({"channel": "#finance-reports"}),
    )
    .unwrap();
    assert_eq!(loc, "slack #finance-reports (C2) message 1700000000.000100");
    let sent = calls_to(&calls, "chat.postMessage");
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].form["channel"], "C2");
    assert_eq!(
        sent[0].form["text"],
        "*Daily \u{2217}revenue\u{2217}*\nRevenue *€12,340* (_+4.1%_)\n• top: acme_corp 2\u{2217}3 &lt;b&gt;\n• <https://x.test/r_1|Report>"
    );
    assert!(calls_to(&calls, "files.getUploadURLExternal").is_empty());
}

#[test]
fn posts_a_message_to_a_dm() {
    let (base, calls) = fake_slack();
    let f = Files::new();
    let loc = post(
        &message(&f, "Hi", "Hello"),
        &[],
        conn(&base, "xoxb-good"),
        json!({"user": "U7"}),
    )
    .unwrap();
    assert_eq!(loc, "slack DM with U7 message 1700000000.000100");
    let sent: Vec<Call> = calls_to(&calls, "chat.postMessage")
        .into_iter()
        .filter(|c| c.form.contains_key("text"))
        .collect();
    assert_eq!(sent[0].form["channel"], "D_U7");
    assert_eq!(sent[0].form["text"], "*Hi*\nHello");
}

#[test]
fn an_over_limit_message_is_cut_short_with_the_full_md_attached() {
    let (base, calls) = fake_slack();
    let f = Files::new();
    let long: String = (0..400).map(|i| format!("line {i} of the message\n")).collect();
    let loc = post(
        &message(&f, "Long", &long),
        &[],
        conn(&base, "xoxb-good"),
        json!({"channel": "C9"}),
    )
    .unwrap();
    assert!(
        loc.starts_with("slack C9: https://acme.slack.com/files/F1"),
        "{loc}"
    );
    assert!(calls_to(&calls, "chat.postMessage").is_empty());
    let get = calls_to(&calls, "files.getUploadURLExternal");
    assert_eq!(get.len(), 1);
    assert_eq!(get[0].form["filename"], "daily.md");
    let done = calls_to(&calls, "files.completeUploadExternal");
    let comment = &done[0].form["initial_comment"];
    assert!(comment.chars().count() <= 4_000, "{}", comment.chars().count());
    assert!(comment.starts_with("*Long*\nline 0 of the message"));
    assert!(comment.ends_with("_(cut short: the full message is attached)_"));
}

#[test]
fn attached_files_go_with_the_message_in_one_post() {
    let (base, calls) = fake_slack();
    let f = Files::new();
    let loc = post(
        &message(&f, "Daily", "See attached"),
        &[f.file("detail.xlsx", b"xlsx")],
        conn(&base, "xoxb-good"),
        json!({"channel": "C9"}),
    )
    .unwrap();
    assert_eq!(loc, "slack C9: https://acme.slack.com/files/F1");
    let done = calls_to(&calls, "files.completeUploadExternal");
    assert_eq!(done[0].form["initial_comment"], "*Daily*\nSee attached");
    let files: Value = serde_json::from_str(&done[0].form["files"]).unwrap();
    assert_eq!(files, json!([{"id": "F1", "title": "detail.xlsx"}]));
}

#[test]
fn a_title_over_the_limit_is_cut_short_too() {
    let (base, calls) = fake_slack();
    let f = Files::new();
    let title = "t".repeat(5_000);
    post(
        &message(&f, &title, "body"),
        &[],
        conn(&base, "xoxb-good"),
        json!({"channel": "C9"}),
    )
    .unwrap();
    let done = calls_to(&calls, "files.completeUploadExternal");
    let comment = &done[0].form["initial_comment"];
    assert!(comment.chars().count() <= 4_000, "{}", comment.chars().count());
    assert!(comment.starts_with("*ttt"), "{comment}");
}

// ---- Block Kit ----

fn post_logged(
    m: &Message,
    attach: &[DeliveryFile],
    conn: Value,
    options: Value,
) -> (Result<String, String>, Vec<String>) {
    let (sink, logs) = logging();
    let mut p = PluginProcess::start(bin(), sink).unwrap();
    let r = p
        .deliver_message(m, attach, obj(conn), obj(options))
        .map_err(|e| e.to_string());
    let _ = p.close();
    std::thread::sleep(std::time::Duration::from_millis(200));
    let l = logs.lock().unwrap().clone();
    (r, l)
}

fn entry(component: &str, blocks: Value) -> Value {
    json!({"component": component, "blocks": blocks})
}

fn points(n: usize) -> Vec<Value> {
    (1..=n)
        .map(|i| json!({"label": format!("d{i:02}"), "value": i}))
        .collect()
}

fn line(series: Value, cats: Vec<String>) -> Value {
    json!({"type": "data_visualization", "title": "Revenue", "chart": {
        "type": "line", "series": series, "axis_config": {"categories": cats}}})
}

fn sent_blocks(calls: &Calls) -> Vec<Value> {
    let sent: Vec<Call> = calls_to(calls, "chat.postMessage")
        .into_iter()
        .filter(|c| c.form.contains_key("text"))
        .collect();
    assert_eq!(sent.len(), 1);
    serde_json::from_str(&sent[0].form["blocks"]).unwrap()
}

#[test]
fn blocks_are_posted_with_the_title_and_text_and_files_go_in_a_thread() {
    let (base, calls) = fake_slack();
    let f = Files::new();
    let (r, logs) = post_logged(
        &message(&f, "Daily", "Revenue **up**"),
        &[f.file("detail.xlsx", b"x")],
        conn(&base, "xoxb-good"),
        json!({"channel": "C9", "blocks": [
            entry("header.json", json!([{"type": "header", "text": {"type": "plain_text", "text": "Hi"}}])),
            {"blocks": [{"type": "divider"}]}
        ]}),
    );
    assert_eq!(
        r.unwrap(),
        "slack C9: https://acme.slack.com/archives/C9/p1700000000000100, https://acme.slack.com/files/F1"
    );
    assert!(logs.is_empty(), "{logs:?}");
    let sent = calls_to(&calls, "chat.postMessage");
    assert_eq!(sent[0].form["text"], "*Daily*\nRevenue *up*");
    let b = sent_blocks(&calls);
    assert_eq!(b[0]["type"], "header");
    assert_eq!(b[1], json!({"type": "divider"}));
    let done = calls_to(&calls, "files.completeUploadExternal");
    assert_eq!(done[0].form["thread_ts"], "1700000000.000100");
    assert!(!done[0].form.contains_key("initial_comment"));
}

#[test]
fn files_in_message_posts_the_files_separately() {
    let (base, calls) = fake_slack();
    let f = Files::new();
    post(
        &message(&f, "Daily", "x"),
        &[f.file("detail.xlsx", b"x")],
        conn(&base, "xoxb-good"),
        json!({"channel": "C9", "files_in": "message", "blocks": [{"blocks": [{"type": "divider"}]}]}),
    )
    .unwrap();
    assert!(
        !calls_to(&calls, "files.completeUploadExternal")[0]
            .form
            .contains_key("thread_ts")
    );
    let err = post(
        &message(&f, "Daily", "x"),
        &[],
        conn(&base, "xoxb-good"),
        json!({"channel": "C9", "files_in": "nowhere"}),
    )
    .unwrap_err();
    assert!(err.contains("files_in"), "{err}");
}

#[test]
fn a_cut_message_attaches_the_full_text_in_the_thread() {
    let (base, calls) = fake_slack();
    let f = Files::new();
    let long: String = (0..400).map(|i| format!("line {i} of the message\n")).collect();
    let (r, logs) = post_logged(
        &message(&f, "Long", &long),
        &[],
        conn(&base, "xoxb-good"),
        json!({"channel": "C9", "blocks": [{"blocks": [{"type": "divider"}]}]}),
    );
    r.unwrap();
    assert!(logs.iter().any(|l| l.contains("4000 characters")), "{logs:?}");
    let text = &calls_to(&calls, "chat.postMessage")[0].form["text"];
    assert!(text.chars().count() <= 4_000);
    let get = calls_to(&calls, "files.getUploadURLExternal");
    assert_eq!(get[0].form["filename"], "daily.md");
    assert_eq!(
        calls_to(&calls, "files.completeUploadExternal")[0].form["thread_ts"],
        "1700000000.000100"
    );
}

#[test]
fn a_file_output_posts_blocks_with_its_message_as_text() {
    let (base, calls) = fake_slack();
    let f = Files::new();
    let loc = send(
        &[f.file("a.csv", b"1")],
        conn(&base, "xoxb-good"),
        json!({"channel": "C9", "message": "Summary", "blocks": [{"blocks": [{"type": "divider"}]}]}),
    )
    .unwrap();
    assert!(
        loc.contains("/archives/C9/") && loc.ends_with("files/F1"),
        "{loc}"
    );
    assert_eq!(calls_to(&calls, "chat.postMessage")[0].form["text"], "Summary");
    assert_eq!(
        calls_to(&calls, "files.completeUploadExternal")[0].form["thread_ts"],
        "1700000000.000100"
    );
}

fn post_chart(block: Value, keep: &str) -> (Vec<Value>, Vec<String>) {
    let (base, calls) = fake_slack();
    let f = Files::new();
    let (r, logs) = post_logged(
        &message(&f, "T", "x"),
        &[],
        conn(&base, "xoxb-good"),
        json!({"channel": "C9", "blocks": [{"component": "line.json", "keep": keep, "blocks": [block]}]}),
    );
    r.unwrap();
    (sent_blocks(&calls), logs)
}

#[test]
fn over_20_points_keep_the_first_or_last_and_warn() {
    let cats: Vec<String> = (1..=25).map(|i| format!("d{i:02}")).collect();
    let block = line(json!([{"name": "rev", "data": points(25)}]), cats);
    let (b, logs) = post_chart(block.clone(), "first");
    let c = &b[0]["chart"];
    assert_eq!(c["axis_config"]["categories"][0], "d01");
    assert_eq!(c["axis_config"]["categories"].as_array().unwrap().len(), 20);
    assert_eq!(c["series"][0]["data"].as_array().unwrap().len(), 20);
    assert_eq!(c["series"][0]["data"][19]["label"], "d20");
    assert!(
        logs.iter()
            .any(|l| l.contains("line.json, block 1") && l.contains("20")),
        "{logs:?}"
    );

    let (b, _) = post_chart(block, "last");
    let c = &b[0]["chart"];
    assert_eq!(c["axis_config"]["categories"][0], "d06");
    assert_eq!(c["series"][0]["data"][0]["label"], "d06");
    assert_eq!(c["series"][0]["data"][19]["label"], "d25");
}

#[test]
fn long_names_cut_missing_points_filled_and_series_limited() {
    let long = "x".repeat(30);
    let mut series: Vec<Value> = (1..=14)
        .map(|i| json!({"name": format!("s{i}"), "data": [{"label": long, "value": 1}]}))
        .collect();
    series[0]["name"] = json!(long);
    series[1]["data"] = json!([]); // a series with no point for the category
    let mut block = line(json!(series), vec![long.clone()]);
    block["title"] = json!("t".repeat(60));
    block["chart"]["axis_config"]["x_label"] = json!("a".repeat(60));
    let (b, logs) = post_chart(block, "first");
    let c = &b[0]["chart"];
    let cut20 = format!("{}…", "x".repeat(19));
    assert_eq!(b[0]["title"], format!("{}…", "t".repeat(49)));
    assert_eq!(c["axis_config"]["x_label"], format!("{}…", "a".repeat(49)));
    assert_eq!(c["axis_config"]["categories"], json!([cut20]));
    assert_eq!(c["series"].as_array().unwrap().len(), 12);
    assert_eq!(c["series"][0]["name"], cut20);
    assert_eq!(c["series"][0]["data"][0]["label"], cut20);
    assert_eq!(c["series"][1]["data"], json!([{"label": cut20, "value": 0}]));
    for needle in [
        "series",
        "filled with 0",
        "title",
        "axis labels",
        "labels or names",
    ] {
        assert!(logs.iter().any(|l| l.contains(needle)), "{needle}: {logs:?}");
    }
}

#[test]
fn pie_values_of_zero_or_less_are_dropped_and_segments_limited() {
    let mut segs: Vec<Value> = (1..=16)
        .map(|i| json!({"label": format!("p{i}"), "value": i}))
        .collect();
    segs[0]["value"] = json!(0);
    segs[1]["value"] = json!(-3);
    let block =
        json!({"type": "data_visualization", "title": "Share", "chart": {"type": "pie", "segments": segs}});
    let (b, logs) = post_chart(block, "first");
    let s = b[0]["chart"]["segments"].as_array().unwrap();
    assert_eq!(s.len(), 12);
    assert_eq!(s[0]["label"], "p3");
    assert!(
        logs.iter().any(|l| l.contains("dropped 2 pie values")),
        "{logs:?}"
    );
    assert!(logs.iter().any(|l| l.contains("14 pie segments")), "{logs:?}");
}

#[test]
fn over_two_charts_or_fifty_blocks_are_trimmed_and_buttons_warned_once() {
    let (base, calls) = fake_slack();
    let f = Files::new();
    let pie = json!({"type": "data_visualization", "title": "P", "chart": {"type": "pie", "segments": [{"label": "a", "value": 1}]}});
    let mut blocks = vec![pie.clone(), pie.clone(), pie];
    blocks.extend(
        (0..60).map(|_| json!({"type": "actions", "elements": [{"type": "button", "action_id": "a"}]})),
    );
    let (r, logs) = post_logged(
        &message(&f, "T", "x"),
        &[],
        conn(&base, "xoxb-good"),
        json!({"channel": "C9", "blocks": [entry("many.json", json!(blocks))]}),
    );
    r.unwrap();
    let sent = sent_blocks(&calls);
    assert_eq!(sent.len(), 50);
    assert_eq!(
        sent.iter().filter(|b| b["type"] == "data_visualization").count(),
        2
    );
    assert!(
        logs.iter()
            .any(|l| l.contains("many.json, block 3") && l.contains("2 charts")),
        "{logs:?}"
    );
    assert!(logs.iter().any(|l| l.contains("Slack allows 50")), "{logs:?}");
    assert_eq!(
        logs.iter().filter(|l| l.contains("interactive")).count(),
        1,
        "{logs:?}"
    );

    // A link button is not interactive.
    let (_, logs) = post_logged(
        &message(&f, "T", "x"),
        &[],
        conn(&base, "xoxb-good"),
        json!({"channel": "C9", "blocks": [{"blocks": [{"type": "actions", "elements": [{"type": "button", "url": "https://x.test", "text": {"type": "plain_text", "text": "Go"}}]}]}]}),
    );
    assert!(logs.is_empty(), "{logs:?}");
}

#[test]
fn invalid_blocks_are_explained_and_bad_options_rejected() {
    let (base, _calls) = fake_slack();
    let f = Files::new();
    let err = post(
        &message(&f, "T", "x"),
        &[],
        conn(&base, "xoxb-good"),
        json!({"channel": "C_BADBLOCKS", "blocks": [{"blocks": [{"type": "divider"}]}]}),
    )
    .unwrap_err();
    assert!(
        err.contains("Slack rejected the blocks")
            && err.contains("bad block")
            && err.contains("Block Kit Builder"),
        "{err}"
    );

    for options in [
        json!({"channel": "C9", "blocks": "nope"}),
        json!({"channel": "C9", "blocks": [{"keep": "middle", "component": "a.json"}]}),
        json!({"channel": "C9", "blocks": [{"nothing": 1}]}),
    ] {
        assert!(post(&message(&f, "T", "x"), &[], conn(&base, "xoxb-good"), options).is_err());
    }
}
