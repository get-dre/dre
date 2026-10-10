//! The `teams` destination: posts messages to a Microsoft Teams channel through a Workflows
//! incoming webhook ("Post to a channel when a webhook request is received"). It takes messages
//! only: Teams can't take a file through a webhook.
//!
//! Profile target field (`profiles.yml`): `webhook_url`, secret, best set with `env_var()`. It's
//! never logged or shown in an error. No destination options.
//!
//! The message goes as an Adaptive Card: the title in bold, then one text block per line of the
//! text, in the Markdown Teams renders. Over the limit, the text is cut short with a marker. A
//! rate-limited post is retried once, after `Retry-After`.

use dre_protocol::delivery::{self, Retry, Rules, retry};
use dre_protocol::markdown;
use dre_protocol::msg::{ConnectionField, Message};
use dre_protocol::plugin::{About, Delivery, Destination, Result, conn_required, serve_destination};
use dre_protocol::{CAP_MESSAGE, CAP_MESSAGE_ONLY};
use serde_json::{Map, Value, json};

/// The longest text posted, in characters: well inside Teams' 28 KB card limit.
const MESSAGE_LIMIT: u64 = 15_000;
const CUT_MARKER: &str = "\n… _(cut short: the full message is in the run's .md file)_";
/// Longest `Retry-After` honoured before giving up on a rate-limited post.
const MAX_RETRY_WAIT: u64 = 60;

struct Teams;

impl Destination for Teams {
    fn connection_fields(&self) -> Vec<ConnectionField> {
        vec![
            ConnectionField::new(
                "webhook_url",
                "the Workflows webhook URL of the channel (keep it secret)",
            )
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
        let (text, cut) = markdown::fit(&m.text, MESSAGE_LIMIT as usize, CUT_MARKER, markdown::to_teams);
        if cut {
            dre_protocol::log::warn!(
                "the message is over the teams limit of {MESSAGE_LIMIT} characters; it was cut short"
            );
        }
        post(url, &card(&m.title, &text), &d.connection)?;
        Ok("teams channel (webhook)".into())
    }

    fn deliver_files(&mut self, _d: &Delivery) -> Result<String> {
        Err(FILES_REFUSED.into())
    }
}

const FILES_REFUSED: &str =
    "the teams destination only takes messages; deliver files to object storage and link them from a message";

/// The Adaptive Card a Workflows webhook posts: the title, then a text block per line.
fn card(title: &str, text: &str) -> Value {
    let mut body = vec![json!({
        "type": "TextBlock",
        "text": markdown::to_teams(&markdown::escape(title)),
        "weight": "Bolder",
        "size": "Medium",
        "wrap": true,
    })];
    let mut gap = false;
    for line in text.lines() {
        if line.trim().is_empty() {
            gap = true;
            continue;
        }
        body.push(json!({
            "type": "TextBlock",
            "text": line,
            "wrap": true,
            "spacing": if gap { "Medium" } else { "None" },
        }));
        gap = false;
    }
    json!({
        "type": "message",
        "attachments": [{
            "contentType": "application/vnd.microsoft.card.adaptive",
            "contentUrl": null,
            "content": {
                "$schema": "http://adaptivecards.io/schemas/adaptive-card.json",
                "type": "AdaptiveCard",
                "version": "1.4",
                "body": body,
            },
        }],
    })
}

/// POST the card. Tried again (`retries`) only when it certainly wasn't posted: a 429 or 503, or no
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
    let done = retry(rules.retries, "Teams webhook", || {
        let resp = agent
            .post(url)
            .header("Content-Type", "application/json")
            .send(body.as_bytes())
            .map_err(|e| {
                let m = format!("can't reach the Teams webhook: {}", scrub(&e.to_string(), url));
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
                    "Teams is rate-limiting posts; try again later".to_string()
                } else {
                    format!("the Teams webhook returned HTTP {status}")
                };
                Err(Retry::Temporary(m, wait))
            }
            429 => Err(Retry::Fail(format!(
                "Teams is rate-limiting posts (retry after {}s); try again later",
                wait.map_or(0, |w| w.as_secs())
            ))),
            400 => Err(Retry::Fail("the Teams webhook rejected the message (HTTP 400); check that the flow uses the \"Post to a channel when a webhook request is received\" template".to_string())),
            401 | 403 | 404 => Err(Retry::Fail(format!(
                "the Teams webhook refused the post (HTTP {status}); the URL may be wrong, or the flow turned off or deleted: check `webhook_url` in the profile"
            ))),
            s => Err(Retry::Fail(format!("the Teams webhook returned HTTP {s}"))),
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

/// `text` with the webhook URL (and its query, which holds the signature) removed.
fn scrub(text: &str, url: &str) -> String {
    let base = url.split('?').next().unwrap_or(url);
    text.replace(url, "<webhook_url>").replace(base, "<webhook_url>")
}

fn main() {
    serve_destination(
        About::new("teams", env!("CARGO_PKG_VERSION")).capabilities(&[CAP_MESSAGE, CAP_MESSAGE_ONLY]),
        Teams,
    )
}
