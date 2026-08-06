//! Model and assembly for the cross-language golden wire corpus.
//!
//! The corpus is one JSON file at the repository root — `corpus/wire-corpus.json`
//! — generated from Rust and consumed as a peer by all three implementations of
//! the Duet wire format: this workspace's `duet-codec`/`duet-protocol`, the Dart
//! guest client, and the JavaScript guest client. It lives outside every
//! language's tree because none of them owns it.
//!
//! # What an entry asserts
//!
//! An **accept** entry pins four things:
//!
//! - `wire` — the exact JSON text a decoder receives.
//! - `witness` — what it must decode to, in the deliberately different
//!   representation described in [`witness`].
//! - `reencodes_to` — the text re-encoding must produce, or `null` meaning
//!   "the same as `wire`". Non-null entries pin the cases where the decoder is
//!   deliberately wider than the encoder.
//! - `reencode_byte_exact` — whether that comparison may be made byte-for-byte.
//!
//! A **reject** entry pins the wire text and the `reason` — not merely *that*
//! the message was refused but *which rule* it broke. Asserting only "is an
//! error" is close to asserting nothing: a mutated codec that rejects every
//! reject case for a brand-new wrong reason passes such a test completely.
//!
//! # Why `reencode_byte_exact` is computed here rather than assumed
//!
//! A self-inverting round-trip test — decode then encode, compare to the input
//! — cannot see an encoder and a decoder that are wrong in the *same*
//! direction. Comparing against text this generator produced does see it. But
//! byte equality is only a fair demand when the canonical encoding contains no
//! JSON-number float payload, because float *rendering* legitimately differs
//! between languages (`1e16` in Rust, `10000000000000000` in JavaScript) while
//! denoting the same double. So Rust computes the flag, and a guest asserts
//! byte equality when it is true and deep structural equality when it is false.
//!
//! # What byte-exactness demands of a guest encoder
//!
//! Every `wire` string here has its JSON **object keys in byte order** —
//! `{"id":"7","kind":"get","path":"a"}`, not the order the fields are declared
//! in. That is not a stylistic choice of this generator: `serde_json::Map` is a
//! `BTreeMap` in this workspace, so it is what the host actually emits, and it
//! is the same rule `duet-codec` already applies to `Value::Map` keys.
//!
//! A guest whose encoder emits keys in *insertion* order — which is what
//! `JSON.stringify` does in JavaScript, and what Dart's `Map` literal plus
//! `jsonEncode` does — will produce equivalent JSON that is not byte-equal, and
//! will fail every `reencode_byte_exact` case. The fix belongs in the guest:
//! build the object with its keys already sorted, or serialize through a
//! sorting writer. Discovering that from a corpus failure is the point; the
//! alternative is two implementations that agree until the day something
//! compares bytes.

pub mod cases;
pub mod check;
pub mod witness;

use std::path::PathBuf;

use duet_codec::CodecError;
use duet_core::Value;
use duet_protocol::{Push, Request, Response};
use serde_json::{Map as JsonMap, Value as Json};

/// Schema version. Bump only for a change a guest reader must adapt to.
pub const VERSION: u64 = 1;

/// The exact command that regenerates the committed file.
pub const GENERATOR: &str =
    "cargo test -p duet-protocol --test wire_corpus -- --ignored regenerate_corpus";

/// Which decoder an entry is addressed to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layer {
    /// A bare tagged value: `duet_codec::decode_value`.
    Value,
    /// A guest-to-host message: `duet_protocol::decode_request`.
    Request,
    /// A host-to-guest reply: `duet_protocol::decode_response`.
    Response,
    /// An unsolicited host-to-guest message: `duet_protocol::decode_push`.
    Push,
}

impl Layer {
    /// The name this layer carries in the corpus file.
    pub fn as_str(self) -> &'static str {
        match self {
            Layer::Value => "value",
            Layer::Request => "request",
            Layer::Response => "response",
            Layer::Push => "push",
        }
    }
}

/// The reason recorded for input that never reaches [`CodecError`] at all,
/// because `serde_json` refuses to parse it first.
///
/// Deliberately not a [`CodecError`] variant — see
/// [`CodecError::reason_code`]'s documentation.
pub const BAD_JSON: &str = "bad_json";

/// A wire text that must decode, and what it must decode to.
#[derive(Debug, Clone)]
pub struct AcceptCase {
    /// Hierarchical name, e.g. `value/float/negative_zero`.
    pub name: String,
    /// Which decoder this is addressed to.
    pub layer: Layer,
    /// The exact JSON text to decode.
    pub wire: String,
    /// What it must decode to.
    pub witness: Json,
    /// What re-encoding must produce, or `None` meaning "the same as `wire`".
    pub reencodes_to: Option<String>,
    /// Whether that comparison may be byte-for-byte.
    pub reencode_byte_exact: bool,
}

/// A wire text that must be refused, and the rule it breaks.
#[derive(Debug, Clone)]
pub struct RejectCase {
    /// Hierarchical name, e.g. `envelope/id/non_canonical`.
    pub name: String,
    /// Which decoder this is addressed to.
    pub layer: Layer,
    /// The exact JSON text to decode.
    pub wire: String,
    /// A [`CodecError::reason_code`], or [`BAD_JSON`].
    pub reason: &'static str,
}

/// Serializes JSON to the compact text the wire actually carries.
pub fn to_text(json: &Json) -> String {
    serde_json::to_string(json).expect("a serde_json::Value always serializes")
}

/// Parses corpus wire text that is known to be well-formed JSON.
pub fn parse(text: &str) -> Json {
    serde_json::from_str(text).unwrap_or_else(|e| panic!("{text} should be valid JSON: {e}"))
}

/// True if `json` contains a JSON number anywhere.
///
/// In this wire format that is equivalent to "contains a float payload spelled
/// as a JSON number": every other numeric field travels as a decimal *string*
/// (`Int` payloads, and the `id`, `subscription` and `subscriber` fields), and
/// the four floats without a portable JSON-number spelling — `NaN`, `±Infinity`
/// and `-0.0` — travel as string sentinels. So a document with no JSON number
/// in it has no language-dependent rendering in it either, and byte equality is
/// a fair thing to demand of every guest.
pub fn contains_json_number(json: &Json) -> bool {
    match json {
        Json::Number(_) => true,
        Json::Array(items) => items.iter().any(contains_json_number),
        Json::Object(map) => map.values().any(contains_json_number),
        _ => false,
    }
}

/// Decodes `json` at `layer`, returning its witness and its canonical
/// re-encoding as text.
///
/// The single decode-and-re-encode path, shared by the generator (which uses it
/// to derive `reencodes_to`) and by the verifier (which uses it to check the
/// committed file against the current code).
///
/// # Errors
///
/// The [`CodecError`] the layer's decoder produced.
pub fn decode_and_reencode(layer: Layer, json: &Json) -> Result<(Json, String), CodecError> {
    match layer {
        Layer::Value => {
            let v = duet_codec::decode_value(json)?;
            Ok((witness::value(&v), to_text(&duet_codec::encode_value(&v))))
        }
        Layer::Request => {
            let r = duet_protocol::decode_request(json)?;
            Ok((
                witness::request(&r),
                to_text(&duet_protocol::encode_request(&r)),
            ))
        }
        Layer::Response => {
            let r = duet_protocol::decode_response(json)?;
            Ok((
                witness::response(&r),
                to_text(&duet_protocol::encode_response(&r)),
            ))
        }
        Layer::Push => {
            let p = duet_protocol::decode_push(json)?;
            Ok((witness::push(&p), to_text(&duet_protocol::encode_push(&p))))
        }
    }
}

/// Builds an accept case from wire text and the witness it is expected to
/// produce, deriving `reencodes_to` and `reencode_byte_exact` from the code.
///
/// Asserts the witness matches at generation time, so a case can never be
/// committed claiming something the codec does not do.
pub fn accept(name: &str, layer: Layer, wire: String, expected: Json) -> AcceptCase {
    let (actual, canonical) = decode_and_reencode(layer, &parse(&wire))
        .unwrap_or_else(|e| panic!("{name}: {wire} should decode, got {e}"));
    assert_eq!(actual, expected, "{name}: witness mismatch for {wire}");
    AcceptCase {
        name: name.to_string(),
        layer,
        reencode_byte_exact: !contains_json_number(&parse(&canonical)),
        reencodes_to: (canonical != wire).then_some(canonical),
        wire,
        witness: expected,
    }
}

/// An accept case for a [`Value`] spelled the way this codec encodes it.
pub fn value_case(name: &str, v: &Value) -> AcceptCase {
    let wire = to_text(&duet_codec::encode_value(v));
    accept(name, Layer::Value, wire, witness::value(v))
}

/// An accept case for a [`Value`] spelled some *other* accepted way.
///
/// The decoder is deliberately wider than the encoder in a few places — a guest
/// hand-building a value cannot always control its JSON spelling. These cases
/// pin that width, and their `reencodes_to` names the canonical form.
pub fn value_spelled_case(name: &str, wire: &str, expected: &Value) -> AcceptCase {
    accept(
        name,
        Layer::Value,
        wire.to_string(),
        witness::value(expected),
    )
}

/// An accept case for a [`Request`].
pub fn request_case(name: &str, r: &Request) -> AcceptCase {
    let wire = to_text(&duet_protocol::encode_request(r));
    accept(name, Layer::Request, wire, witness::request(r))
}

/// An accept case for a [`Response`].
pub fn response_case(name: &str, r: &Response) -> AcceptCase {
    let wire = to_text(&duet_protocol::encode_response(r));
    accept(name, Layer::Response, wire, witness::response(r))
}

/// An accept case for a [`Push`].
pub fn push_case(name: &str, p: &Push) -> AcceptCase {
    let wire = to_text(&duet_protocol::encode_push(p));
    accept(name, Layer::Push, wire, witness::push(p))
}

/// A reject case.
pub fn reject(
    name: &str,
    layer: Layer,
    wire: impl Into<String>,
    reason: &'static str,
) -> RejectCase {
    RejectCase {
        name: name.to_string(),
        layer,
        wire: wire.into(),
        reason,
    }
}

impl AcceptCase {
    /// This case as the JSON object the corpus file stores.
    fn to_json(&self) -> Json {
        let mut m = JsonMap::new();
        m.insert("name".to_string(), Json::String(self.name.clone()));
        m.insert(
            "layer".to_string(),
            Json::String(self.layer.as_str().into()),
        );
        m.insert("wire".to_string(), Json::String(self.wire.clone()));
        m.insert("witness".to_string(), self.witness.clone());
        m.insert(
            "reencodes_to".to_string(),
            match &self.reencodes_to {
                Some(text) => Json::String(text.clone()),
                None => Json::Null,
            },
        );
        m.insert(
            "reencode_byte_exact".to_string(),
            Json::Bool(self.reencode_byte_exact),
        );
        Json::Object(m)
    }
}

impl RejectCase {
    /// This case as the JSON object the corpus file stores.
    fn to_json(&self) -> Json {
        let mut m = JsonMap::new();
        m.insert("name".to_string(), Json::String(self.name.clone()));
        m.insert(
            "layer".to_string(),
            Json::String(self.layer.as_str().into()),
        );
        m.insert("wire".to_string(), Json::String(self.wire.clone()));
        m.insert("reason".to_string(), Json::String(self.reason.to_string()));
        Json::Object(m)
    }
}

/// Builds the whole corpus document.
pub fn document() -> Json {
    let accept: Vec<Json> = cases::accept_cases()
        .iter()
        .map(AcceptCase::to_json)
        .collect();
    let reject: Vec<Json> = cases::reject_cases()
        .iter()
        .map(RejectCase::to_json)
        .collect();
    let mut m = JsonMap::new();
    m.insert("version".to_string(), Json::Number(VERSION.into()));
    m.insert("generator".to_string(), Json::String(GENERATOR.to_string()));
    m.insert("accept".to_string(), Json::Array(accept));
    m.insert("reject".to_string(), Json::Array(reject));
    Json::Object(m)
}

/// The exact bytes the committed file must contain.
///
/// `serde_json::Map` is a `BTreeMap` in this workspace (the `preserve_order`
/// feature is off), so object keys come out in one deterministic order and the
/// snapshot is stable. Pretty-printed with a trailing newline so the diff is
/// reviewable and the file is a well-behaved text file.
pub fn render() -> String {
    let mut text =
        serde_json::to_string_pretty(&document()).expect("the corpus document always serializes");
    text.push('\n');
    text
}

/// Where the committed corpus lives: the repository root, not any one
/// language's tree, because all three implementations consume it as peers.
pub fn corpus_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("corpus")
        .join("wire-corpus.json")
}
