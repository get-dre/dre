//! Teams delivery against a local fake of a Workflows webhook.

fn bin() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_dre-plugin-teams"))
}

const LIMIT: u64 = 15_000;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::{Arc, Mutex};

use dre_protocol::host::{LogSink, PluginProcess};
use dre_protocol::msg::{DeliveryFile, Message};
use dre_protocol::{CAP_MESSAGE, CAP_MESSAGE_ONLY, conformance};
use serde_json::{Map, Value, json};

/// Each request the fake received: path and JSON body.
type Posts = Arc<Mutex<Vec<(String, Value)>>>;

/// A webhook server. Paths: `/slow...` is rate-limited once (Retry-After: 1), `/busy...` always
/// (Retry-After: 120), `/gone...` is 404; anything else is accepted.
fn fake_webhook() -> (String, Posts) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let posts: Posts = Arc::default();
    let p = posts.clone();
    std::thread::spawn(move || {
        let mut slow_once = true;
        for stream in listener.incoming().flatten() {
            let mut r = BufReader::new(stream.try_clone().unwrap());
            let mut s = stream;
            let mut line = String::new();
            if r.read_line(&mut line).unwrap_or(0) == 0 {
                continue;
            }
            let path = line.split_whitespace().nth(1).unwrap().to_string();
            let mut len = 0usize;
            loop {
                let mut h = String::new();
                r.read_line(&mut h).unwrap();
                let h = h.trim_end().to_string();
                if h.is_empty() {
                    break;
                }
                if let Some((k, v)) = h.split_once(':')
                    && k.eq_ignore_ascii_case("content-length")
                {
                    len = v.trim().parse().unwrap();
                }
            }
            let mut body = vec![0; len];
            r.read_exact(&mut body).unwrap();
            p.lock()
                .unwrap()
                .push((path.clone(), serde_json::from_slice(&body).unwrap_or(Value::Null)));
            let (status, extra) = if path.starts_with("/busy") {
                ("429 Too Many Requests", "Retry-After: 120\r\n")
            } else if path.starts_with("/slow") && std::mem::take(&mut slow_once) {
                ("429 Too Many Requests", "Retry-After: 1\r\n")
            } else if path.starts_with("/gone") {
                ("404 Not Found", "")
            } else {
                ("202 Accepted", "")
            };
            let _ = write!(
                s,
                "HTTP/1.1 {status}\r\n{extra}Content-Length: 0\r\nConnection: close\r\n\r\n"
            );
        }
    });
    (base, posts)
}

fn quiet() -> LogSink {
    Arc::new(|_, _| {})
}

fn conn(url: &str) -> Map<String, Value> {
    json!({"webhook_url": url}).as_object().unwrap().clone()
}

fn message(title: &str, text: &str) -> Message {
    Message {
        title: title.into(),
        text: text.into(),
        html: None,
        path: "/nonexistent/daily.md".into(),
    }
}

fn post(m: &Message, url: &str) -> Result<String, String> {
    let mut p = PluginProcess::start(bin(), quiet()).unwrap();
    let r = p
        .deliver_message(m, &[], conn(url), Map::new())
        .map_err(|e| e.to_string());
    let _ = p.close();
    r
}

#[test]
fn conforms_to_the_protocol() {
    conformance::assert_conforms(bin());
}

#[test]
fn takes_only_messages_and_declares_its_limit() {
    let mut p = PluginProcess::start(bin(), quiet()).unwrap();
    assert!(p.has(CAP_MESSAGE));
    assert!(p.has(CAP_MESSAGE_ONLY));
    assert_eq!(p.description().unwrap().message_limit, Some(LIMIT));
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("a.csv");
    std::fs::write(&f, "n").unwrap();
    let err = p
        .deliver_files(
            &[DeliveryFile {
                local_path: f.to_string_lossy().to_string(),
                remote_path: None,
            }],
            conn("http://127.0.0.1:1/x"),
            Map::new(),
        )
        .unwrap_err()
        .to_string();
    assert!(err.contains("only takes messages"), "{err}");
}

#[test]
fn retries_once_when_rate_limited() {
    let (base, posts) = fake_webhook();
    post(&message("T", "x"), &format!("{base}/slow/hook")).unwrap();
    assert_eq!(posts.lock().unwrap().len(), 2);
    let err = post(&message("T", "x"), &format!("{base}/busy/hook")).unwrap_err();
    assert!(err.contains("rate-limiting"), "{err}");
}

#[test]
fn the_webhook_url_never_appears_in_errors() {
    let (base, _) = fake_webhook();
    let err = post(&message("T", "x"), &format!("{base}/gone/hook?sig=SECRET123")).unwrap_err();
    assert!(err.contains("check `webhook_url`"), "{err}");
    assert!(!err.contains("SECRET123") && !err.contains("/gone/hook"), "{err}");
    let err = post(&message("T", "x"), "http://127.0.0.1:1/hook?sig=SECRET123").unwrap_err();
    assert!(!err.contains("SECRET123") && !err.contains("/hook"), "{err}");
}

fn texts(card: &Value) -> Vec<String> {
    card["attachments"][0]["content"]["body"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["text"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn posts_an_adaptive_card_with_the_title_and_one_block_per_line() {
    let (base, posts) = fake_webhook();
    let loc = post(
        &message(
            "Daily *revenue*",
            "Revenue **€12,340** (_+4.1%_)\n\n- acme\\_corp\n- [Report](https://x.test/r)",
        ),
        &format!("{base}/hook"),
    )
    .unwrap();
    assert_eq!(loc, "teams channel (webhook)");
    let posts = posts.lock().unwrap();
    let card = &posts[0].1;
    assert_eq!(card["type"], "message");
    assert_eq!(
        card["attachments"][0]["contentType"],
        "application/vnd.microsoft.card.adaptive"
    );
    assert_eq!(
        texts(card),
        [
            "Daily \\*revenue\\*",
            "Revenue **€12,340** (_+4.1%_)",
            "- acme\\_corp",
            "- [Report](https://x.test/r)"
        ]
    );
    let body = card["attachments"][0]["content"]["body"].as_array().unwrap();
    assert_eq!(body[0]["weight"], "Bolder");
    assert_eq!(body[2]["spacing"], "Medium");
    assert_eq!(body[3]["spacing"], "None");
}

#[test]
fn an_over_limit_message_is_cut_short_with_a_marker() {
    let (base, posts) = fake_webhook();
    let long: String = (0..2_000).map(|i| format!("line {i} of the message\n")).collect();
    post(&message("Long", &long), &format!("{base}/hook")).unwrap();
    let posts = posts.lock().unwrap();
    let t = texts(&posts[0].1);
    assert!(t.last().unwrap().contains("cut short"), "{:?}", t.last());
    let total: usize = t[1..].iter().map(|l| l.chars().count() + 1).sum();
    assert!(total <= LIMIT as usize + 1, "{total}");
}
