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

use dre_protocol::delivery;
use dre_protocol::markdown;
use dre_protocol::msg::{ConnectionField, Message};
use dre_protocol::plugin::{About, Delivery, Destination, Result, conn_required, serve_destination};
use dre_protocol::{CAP_MESSAGE, CAP_MESSAGE_ONLY};
use serde_json::{Map, Value, json};

/// The longest text posted, in characters.
const MESSAGE_LIMIT: u64 = 15_000;
/// Teams caps a card at 28 KB; a character takes up to 4 bytes, so the card is measured too.
const CARD_BYTES: usize = 24_000;
const CUT_MARKER: &str = "\n… _(cut short: the full message is in the run's .md file)_";

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
        let mut limit = MESSAGE_LIMIT as usize;
        let (card, cut) = loop {
            let (text, cut) = markdown::fit(&m.text, limit, CUT_MARKER, markdown::to_teams);
            let card = card(&m.title, &text);
            let bytes = card.to_string().len();
            if bytes <= CARD_BYTES || limit == 0 {
                break (card, cut);
            }
            // Shrink in proportion to the overshoot (at least one character) and fit again.
            limit = (limit * CARD_BYTES / bytes).min(limit - 1);
        };
        if cut {
            dre_protocol::log::warn!(
                "the message is over the teams limit of {MESSAGE_LIMIT} characters or {CARD_BYTES} bytes; it was cut short"
            );
        }
        post(url, &card, &d.connection)?;
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
    dre_webhook::post(url, payload, connection, "Teams", "application/json", |status| {
        match status {
            400 => "the Teams webhook rejected the message (HTTP 400); check that the flow uses the \"Post to a channel when a webhook request is received\" template".to_string(),
            _ => format!("the Teams webhook refused the post (HTTP {status}); the URL may be wrong, or the flow turned off or deleted: check `webhook_url` in the profile"),
        }
    })
}

fn main() {
    serve_destination(
        About::new("teams", env!("CARGO_PKG_VERSION")).capabilities(&[CAP_MESSAGE, CAP_MESSAGE_ONLY]),
        Teams,
    )
}
