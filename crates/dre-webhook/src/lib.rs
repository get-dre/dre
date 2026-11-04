//! Shared transport and safe retries for HTTP webhook destinations.

use dre_protocol::delivery::{self, Retry, Rules, retry_rules};
use dre_protocol::plugin::Result;
use serde_json::{Map, Value};

/// Longest `Retry-After` honoured before giving up on a rate-limited post.
const MAX_RETRY_WAIT: u64 = 60;

/// POST a JSON payload. Tried again (`retries`) only when it certainly wasn't posted: a 429 or 503, or no
/// connection made. Errors never include the URL, which is a credential.
pub fn post(
    url: &str,
    payload: &Value,
    connection: &Map<String, Value>,
    service: &str,
    content_type: &str,
    rejection: impl Fn(u16) -> String,
) -> Result<()> {
    let (rules, _) = Rules::from_settings(Rules::default(), connection, &Map::new(), &[])?;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_connect(Some(rules.connect_timeout))
        .timeout_recv_response(Some(rules.timeout))
        .build()
        .into();
    let body = payload.to_string();
    let done = retry_rules(&rules, &format!("{service} webhook"), || {
        let resp = agent
            .post(url)
            .header("Content-Type", content_type)
            .send(body.as_bytes())
            .map_err(|e| {
                let m = format!(
                    "can't reach the {service} webhook: {}",
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
                    format!("{service} is rate-limiting posts; try again later")
                } else {
                    format!("the {service} webhook returned HTTP {status}")
                };
                Err(Retry::Temporary(m, wait))
            }
            429 => Err(Retry::Fail(format!(
                "{service} is rate-limiting posts (retry after {}s); try again later",
                wait.map_or(0, |w| w.as_secs())
            ))),
            400 | 401 | 403 | 404 => Err(Retry::Fail(rejection(status))),
            s => Err(Retry::Fail(format!("the {service} webhook returned HTTP {s}"))),
        }
    });
    done.map_err(Into::into)
}

/// Whether a request certainly never reached the server (no connection was made), so trying
/// again can't post twice.
pub fn never_sent(e: &ureq::Error) -> bool {
    matches!(
        e,
        ureq::Error::HostNotFound
            | ureq::Error::ConnectionFailed
            | ureq::Error::Timeout(ureq::Timeout::Resolve | ureq::Timeout::Connect)
    ) || matches!(e, ureq::Error::Io(io) if io.kind() == std::io::ErrorKind::ConnectionRefused)
}

/// `text` with the webhook URL (and its query, which holds credentials) removed.
pub fn scrub(text: &str, url: &str) -> String {
    let base = url.split('?').next().unwrap_or(url);
    text.replace(url, "<webhook_url>").replace(base, "<webhook_url>")
}
