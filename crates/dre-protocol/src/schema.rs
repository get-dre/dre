//! The protocol as data: a JSON Schema for every message, generated from [`crate::msg`], with the
//! protocol version range, the capability names and the error kinds. It is published as
//! `conformance/schema.json` beside the conformance cases, so a plugin written in any language
//! can check its messages and constants against it; a test keeps the file in step with the code.

use schemars::generate::SchemaSettings;
use serde_json::{Value, json};

use crate::msg::{ERROR_KINDS, Envelope, Request, Response};
use crate::{CAPABILITIES, CORE_MIN_VERSION, MAX_VERSION, MIN_VERSION};

/// The published schema file, as checked in.
pub const PUBLISHED: &str = include_str!("../conformance/schema.json");

/// The schema document: `$defs.Request` is any message core sends, `$defs.Response` any message
/// a plugin sends, and `x-dre` the version range, capabilities and error kinds.
#[must_use]
pub fn document() -> Value {
    let mut generator = SchemaSettings::draft2020_12().into_generator();
    generator.subschema_for::<Envelope<Request>>();
    generator.subschema_for::<Envelope<Response>>();
    let defs: serde_json::Map<String, Value> = generator.take_definitions(true).into_iter().collect();
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "DRE plugin protocol messages",
        "description": "Every JSON frame of the DRE plugin protocol (docs/protocol.md). A frame core sends matches `$defs/Request`; a frame a plugin sends matches `$defs/Response`. Generated from dre-protocol's msg.rs; don't edit by hand.",
        "anyOf": [{"$ref": "#/$defs/Request"}, {"$ref": "#/$defs/Response"}],
        "x-dre": {
            "protocol": {
                "min_version": MIN_VERSION,
                "max_version": MAX_VERSION,
                "core_min_version": CORE_MIN_VERSION,
            },
            "capabilities": CAPABILITIES,
            "error_kinds": ERROR_KINDS,
        },
        "$defs": defs,
    })
}

/// The schema document as the published file spells it.
///
/// # Panics
///
/// Never in practice: a `serde_json::Value` always serialises.
#[must_use]
pub fn render() -> String {
    let mut s = serde_json::to_string_pretty(&document()).expect("a schema serialises");
    s.push('\n');
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CI fails here when `msg.rs` changes without the published file. Regenerate it (and the
    /// Go SDK's copies of the conformance files) with
    /// `DRE_UPDATE_SCHEMA=1 cargo test -p dre-protocol --lib schema`.
    #[test]
    fn the_published_schema_matches_the_code() {
        let fresh = render();
        if std::env::var_os("DRE_UPDATE_SCHEMA").is_some() {
            let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("conformance");
            std::fs::write(dir.join("schema.json"), &fresh).unwrap();
            // The Go SDK's plugintest embeds a copy of every published file.
            let go = dir.join("../../../go/plugin/plugintest/conformance");
            for name in ["schema.json", "protocol.json", "delivery.json"] {
                std::fs::copy(dir.join(name), go.join(name)).unwrap();
            }
            return;
        }
        assert!(
            fresh == PUBLISHED,
            "conformance/schema.json is out of date with msg.rs; regenerate it with \
             `DRE_UPDATE_SCHEMA=1 cargo test -p dre-protocol --lib schema`"
        );
    }

    #[test]
    fn every_message_names_its_type() {
        let doc = document();
        for side in ["Request", "Response"] {
            let text = doc["$defs"][side].to_string();
            assert!(text.contains("\"type\""), "{side}: {text}");
        }
        assert_eq!(doc["x-dre"]["protocol"]["max_version"], MAX_VERSION);
    }
}
