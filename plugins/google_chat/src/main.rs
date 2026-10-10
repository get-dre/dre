//! The `google_chat` destination: posts messages to a Google Chat space through its incoming
//! webhook. It takes messages only: a webhook can't carry a file.
//!
//! Profile target field (`profiles.yml`): `webhook_url`, secret, best set with `env_var()`. It's
//! never logged or shown in an error. No destination options.
//!
//! The message is posted as text: the title in bold, then the text translated to Google Chat's
//! formatting. Over the limit, the text is cut short with a marker. A rate-limited post is retried
//! once, after `Retry-After`.

use std::time::Duration;

use dre_protocol::markdown;
use dre_protocol::msg::{ConnectionField, Message};
use dre_protocol::plugin::{About, Delivery, Destination, Result, conn_required, serve_destination};
use dre_protocol::{CAP_MESSAGE, CAP_MESSAGE_ONLY};
use serde_json::{Value, json};

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
        post(url, &json!({ "text": text }))?;
        Ok("google_chat space (webhook)".into())
    }

    fn deliver_files(&mut self, _d: &Delivery) -> Result<String> {
        Err(FILES_REFUSED.into())
    }
}

const FILES_REFUSED: &str = "the google_chat destination only takes messages; deliver files to object storage and link them from a message";

/// POST the message; retry once on 429. Errors never include the URL, which holds the key and
/// token.
fn post(url: &str, payload: &Value) -> Result<()> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(60)))
        .build()
        .into();
    let body = payload.to_string();
    for attempt in 0..2 {
        let resp = agent
            .post(url)
            .header("Content-Type", "application/json; charset=UTF-8")
            .send(body.as_bytes())
            .map_err(|e| {
                format!(
                    "can't reach the Google Chat webhook: {}",
                    scrub(&e.to_string(), url)
                )
            })?;
        let status = resp.status().as_u16();
        match status {
            200..=299 => return Ok(()),
            429 => {
                let wait = resp
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.trim().parse::<u64>().ok())
                    .unwrap_or(1);
                if attempt == 0 && wait <= MAX_RETRY_WAIT {
                    dre_protocol::log::debug!("Google Chat rate-limited the post; retrying in {wait}s");
                    std::thread::sleep(Duration::from_secs(wait));
                    continue;
                }
                return Err(format!(
                    "Google Chat is rate-limiting posts (retry after {wait}s); try again later"
                )
                .into());
            }
            400 => return Err("the Google Chat webhook rejected the message (HTTP 400)".into()),
            401 | 403 | 404 => {
                return Err(format!(
                    "the Google Chat webhook refused the post (HTTP {status}); the URL may be wrong or the webhook deleted: check `webhook_url` in the profile"
                )
                .into());
            }
            s => return Err(format!("the Google Chat webhook returned HTTP {s}").into()),
        }
    }
    unreachable!("the loop returns on its second attempt")
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
