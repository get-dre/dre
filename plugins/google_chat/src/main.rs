//! The `google_chat` destination: posts messages to a Google Chat space through its incoming
//! webhook. It takes messages only: a webhook can't carry a file.
//!
//! Profile target field (`profiles.yml`): `webhook_url`, secret, best set with `env_var()`. It's
//! never logged or shown in an error. No destination options.
//!
//! The message is posted as text: the title in bold, then the text translated to Google Chat's
//! formatting. Over the limit, the text is cut short with a marker. A rate-limited post is retried
//! once, after `Retry-After`.

use dre_protocol::delivery::{self, Retry, Rules, retry};
use dre_protocol::markdown;
use dre_protocol::msg::{ConnectionField, Message};
use dre_protocol::plugin::{About, Delivery, Destination, Result, conn_required, serve_destination};
use dre_protocol::{CAP_MESSAGE, CAP_MESSAGE_ONLY};
use serde_json::{Map, Value, json};

/// The longest text posted, in characters: Google Chat takes 4,096.
const MESSAGE_LIMIT: u64 = 4_000;
const CUT_MARKER: &str = "\n… _(cut short: the full message is in the run's .md file)_";
/// Longest `Retry-After` honoured before giving up on a rate-limited post.
const MAX_RETRY_WAIT: u64 = 60;

struct GoogleChat;

impl Destination for GoogleChat {
    fn connection_fields(&self) -> Vec<ConnectionField> {
        vec![
            ConnectionField::new("webhook_url", "the space's webhook URL (keep it secret)")
                .required()
                .secret(),
        ]
        .into_iter()
        .chain(delivery::connection_fields())
        .collect()
    }

    fn message_limit(&self) -> Option<u64> {
        Some(MESSAGE_LIMIT)
    }

    fn deliver_message(&mut self, d: &Delivery, m: &Message) -> Result<String> {
        if !d.files.is_empty() {
            return Err(FILES_REFUSED.into());
        }
        let url = conn_required(&d.connection, "webhook_url")?;
        let title = markdown::to_google_chat(&markdown::escape(&m.title));
        let title = format!("*{title}*");
        let room = (MESSAGE_LIMIT as usize).saturating_sub(title.chars().count() + 1);
        let (text, cut) = markdown::fit(&m.text, room, CUT_MARKER, markdown::to_google_chat);
        if cut {
            dre_protocol::log::warn!(
                "the message is over the google_chat limit of {MESSAGE_LIMIT} characters; it was cut short"
            );
        }
        let text = format!("{title}\n{text}");
        post(url, &json!({ "text": text }), &d.connection)?;
        Ok("google_chat space (webhook)".into())
    }

    fn deliver_files(&mut self, _d: &Delivery) -> Result<String> {
        Err(FILES_REFUSED.into())
    }
}

const FILES_REFUSED: &str = "the google_chat destination only takes messages; deliver files to object storage and link them from a message";

/// POST the message. Tried again (`retries`) only when it certainly wasn't posted: a 429 or 503, or no
/// connection made. Errors never include the URL, which is a credential.
fn post(url: &str, payload: &Value, connection: &Map<String, Value>) -> Result<()> {
    let (rules, _) = Rules::from_settings(Rules::default(), connection, &Map::new(), &[])?;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_connect(Some(rules.connect_timeout))
        .timeout_recv_response(Some(rules.timeout))
        .build()
        .into();
    let body = payload.to_string();
    let done = retry(rules.retries, "Google Chat webhook", || {
        let resp = agent
            .post(url)
            .header("Content-Type", "application/json; charset=UTF-8")
            .send(body.as_bytes())
            .map_err(|e| {
                let m = format!(
                    "can't reach the Google Chat webhook: {}",
                    scrub(&e.to_string(), url)
                );
                if never_sent(&e) {
                    Retry::Temporary(m, None)
                } else {
                    Retry::Fail(m)
                }
            })?;
        let status = resp.status().as_u16();
        let wait = delivery::retry_after(resp.headers().get("retry-after").and_then(|v| v.to_str().ok()));
        match status {
            200..=299 => Ok(()),
            429 | 503 if wait.is_none_or(|w| w.as_secs() <= MAX_RETRY_WAIT) => {
                let m = if status == 429 {
                    "Google Chat is rate-limiting posts; try again later".to_string()
                } else {
                    format!("the Google Chat webhook returned HTTP {status}")
                };
                Err(Retry::Temporary(m, wait))
            }
            429 => Err(Retry::Fail(format!(
                "Google Chat is rate-limiting posts (retry after {}s); try again later",
                wait.map_or(0, |w| w.as_secs())
            ))),
            400 => Err(Retry::Fail(
                "the Google Chat webhook rejected the message (HTTP 400)".to_string(),
            )),
            401 | 403 | 404 => Err(Retry::Fail(format!(
                "the Google Chat webhook refused the post (HTTP {status}); the URL may be wrong or the webhook deleted: check `webhook_url` in the profile"
            ))),
            s => Err(Retry::Fail(format!("the Google Chat webhook returned HTTP {s}"))),
        }
    });
    done.map_err(Into::into)
}

/// Whether a request certainly never reached the server (no connection was made), so trying
/// again can't post twice.
fn never_sent(e: &ureq::Error) -> bool {
    matches!(
        e,
        ureq::Error::HostNotFound
            | ureq::Error::ConnectionFailed
            | ureq::Error::Timeout(ureq::Timeout::Resolve | ureq::Timeout::Connect)
    ) || matches!(e, ureq::Error::Io(io) if io.kind() == std::io::ErrorKind::ConnectionRefused)
}

/// `text` with the webhook URL (and its query, which holds the key and token) removed.
fn scrub(text: &str, url: &str) -> String {
    let base = url.split('?').next().unwrap_or(url);
    text.replace(url, "<webhook_url>").replace(base, "<webhook_url>")
}

fn main() {
    serve_destination(
        About::new("google_chat", env!("CARGO_PKG_VERSION")).capabilities(&[CAP_MESSAGE, CAP_MESSAGE_ONLY]),
        GoogleChat,
    )
}
