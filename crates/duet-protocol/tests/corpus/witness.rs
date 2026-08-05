//! The corpus's **witness** representation.
//!
//! A corpus entry says "this wire text must decode to *this* value". The
//! witness is how that expectation is written down, and it is deliberately a
//! *different* encoding from the wire itself.
//!
//! # Why not just restate the wire JSON
//!
//! If the expectation were JSON of the same shape as the wire, a guest could
//! satisfy every accept case by parsing the wire text and echoing it back
//! unchanged — never exercising its decoder at all. Every entry would pass in
//! an implementation that has no `Value` type. Making the witness structurally
//! different means the only way to produce it is to actually decode.
//!
//! # Two choices carry the weight
//!
//! **Floats are IEEE-754 bits in hex, never decimal.** Float *text* is not
//! comparable across languages: Rust renders `1e16` as `1e16` and JavaScript
//! renders the same double as `10000000000000000`. Bits make `-0.0`, `NaN`,
//! subnormals and `0.1` exact and unambiguous. It also makes the corpus
//! comparison *stricter than `f64` equality*: `NaN` compares equal to `NaN`
//! (which `==` does not), and `-0.0` compares unequal to `0.0` (which `==` does
//! not either) — so a decoder that dropped the sign bit cannot pass.
//!
//! **Strings are UTF-8 byte arrays.** Escaping, Unicode normalisation and
//! surrogate handling cannot creep into a comparison between two byte arrays.
//!
//! Map keys and paths stay JSON strings. For keys it is their *order* that is
//! under test, not their spelling, and both round-trip as strings; for paths,
//! `duet-core` already proves `Path::parse` and `Display` are mutually inverse,
//! so a second representation would be a second thing to keep in sync.

use duet_core::{Notification, Value};
use duet_protocol::{Push, Request, Response};
use serde_json::{Value as Json, json};

/// Lowercase hex, two characters per byte.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The witness for a [`Value`].
pub fn value(v: &Value) -> Json {
    match v {
        Value::Null => json!({"k": "null"}),
        Value::Bool(b) => json!({"k": "bool", "v": b}),
        // A decimal string, so an i64 past 2^53 stays exact in a JavaScript
        // reader — the same reason the wire itself carries ints as strings.
        Value::Int(i) => json!({"k": "int", "v": i.to_string()}),
        Value::Float(f) => json!({"k": "float", "bits": format!("{:016x}", f.to_bits())}),
        Value::Str(s) => json!({"k": "str", "utf8": s.as_bytes()}),
        Value::Bytes(b) => json!({"k": "bytes", "hex": hex(b)}),
        Value::List(items) => {
            json!({"k": "list", "items": items.iter().map(value).collect::<Vec<_>>()})
        }
        Value::Map(entries) => {
            // `BTreeMap` iteration is byte order over the UTF-8 keys, which is
            // exactly code-point order and exactly what the canonical encoding
            // emits. Pinning it as an ordered array (rather than a JSON object,
            // whose key order no parser is obliged to preserve) is what lets a
            // guest assert the order at all.
            let entries: Vec<Json> = entries.iter().map(|(k, v)| json!([k, value(v)])).collect();
            json!({"k": "map", "entries": entries})
        }
    }
}

/// The witness for an optional value: JSON `null` means the path was **absent**.
///
/// Deliberately distinct from `{"k":"null"}`, which is a path holding
/// [`Value::Null`]. The wire draws the same distinction (`null` versus
/// `{"t":"n"}`), and a guest that collapsed the two would lose it.
fn optional(v: &Option<Value>) -> Json {
    match v {
        Some(v) => value(v),
        None => Json::Null,
    }
}

/// The message a new variant should carry when it reaches a witness function
/// with no arm for it.
///
/// `Request`, `Response` and `Push` are `#[non_exhaustive]`, so a match written
/// here — outside the defining crate — cannot be exhaustive. A new variant
/// therefore fails loudly at corpus generation rather than being encoded wrong.
const UNWITNESSED: &str = "no witness for this variant; add one and regenerate the corpus";

/// The witness for a [`Request`].
pub fn request(r: &Request) -> Json {
    match r {
        Request::Get { id, path } => {
            json!({"k": "get", "id": id.0.to_string(), "path": path.to_string()})
        }
        Request::Set { id, path, value: v } => {
            json!({"k": "set", "id": id.0.to_string(), "path": path.to_string(), "value": value(v)})
        }
        Request::Subscribe { id, path } => {
            json!({"k": "subscribe", "id": id.0.to_string(), "path": path.to_string()})
        }
        Request::Unsubscribe { id, subscription } => json!({
            "k": "unsubscribe",
            "id": id.0.to_string(),
            "subscription": subscription.0.to_string(),
        }),
        other => panic!("{UNWITNESSED}: {other:?}"),
    }
}

/// The witness for a [`Response`].
pub fn response(r: &Response) -> Json {
    match r {
        Response::Value { id, value: v } => {
            json!({"k": "value", "id": id.0.to_string(), "value": optional(v)})
        }
        Response::Done { id } => json!({"k": "done", "id": id.0.to_string()}),
        Response::Subscribed {
            id,
            subscription,
            snapshot,
        } => json!({
            "k": "subscribed",
            "id": id.0.to_string(),
            "subscription": subscription.0.to_string(),
            "snapshot": optional(snapshot),
        }),
        // The failure message is a guest-visible string, so it gets the same
        // UTF-8-bytes treatment as any other string.
        Response::Failed { id, message } => json!({
            "k": "failed",
            "id": id.0.to_string(),
            "message": value(&Value::Str(message.clone())),
        }),
        other => panic!("{UNWITNESSED}: {other:?}"),
    }
}

/// The witness for a [`Notification`].
fn notification(n: &Notification) -> Json {
    json!({
        "k": "notification",
        "subscriber": n.subscriber.0.to_string(),
        "subscription": n.subscription.0.to_string(),
        "path": n.patch.path.to_string(),
        "value": value(&n.patch.value),
    })
}

/// The witness for a [`Push`].
pub fn push(p: &Push) -> Json {
    match p {
        Push::Notification(n) => notification(n),
        other => panic!("{UNWITNESSED}: {other:?}"),
    }
}
