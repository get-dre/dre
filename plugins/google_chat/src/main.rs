//! The `google_chat` destination: posts messages to a Google Chat space through its incoming
//! webhook. It takes messages only: a webhook can't carry a file.
//!
//! Profile target field (`profiles.yml`): `webhook_url`, secret, best set with `env_var()`. It's
//! never logged or shown in an error. No destination options.
//!
//! The message is posted as text: the title in bold, then the text translated to Google Chat's
//! formatting. Over the limit, the text is cut short with a marker. A rate-limited post is retried
//! once, after `Retry-After`.

use dre_protocol::delivery;
use dre_protocol::markdown;
use dre_protocol::msg::{ConnectionField, Message};
use dre_protocol::plugin::{About, Delivery, Destination, Result, conn_required, serve_destination};
use dre_protocol::{CAP_MESSAGE, CAP_MESSAGE_ONLY};
use serde_json::{Map, Value, json};

/// The longest text posted, in characters: Google Chat takes 4,096.
const MESSAGE_LIMIT: u64 = 4_000;
const CUT_MARKER: &str = "\n… _(cut short: the full message is in the run's .md file)_";

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
    dre_webhook::post(
        url,
        payload,
        connection,
        "Google Chat",
        "application/json; charset=UTF-8",
        |status| match status {
            400 => "the Google Chat webhook rejected the message (HTTP 400)".to_string(),
            _ => format!(
                "the Google Chat webhook refused the post (HTTP {status}); the URL may be wrong or the webhook deleted: check `webhook_url` in the profile"
            ),
        },
    )
}

fn main() {
    serve_destination(
        About::new("google_chat", env!("CARGO_PKG_VERSION")).capabilities(&[CAP_MESSAGE, CAP_MESSAGE_ONLY]),
        GoogleChat,
    )
}
