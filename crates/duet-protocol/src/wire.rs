//! Encoding and decoding for the message envelope.

use duet_codec::CodecError;
use duet_core::SubscriptionId;
use serde_json::{Map as JsonMap, Value as Json};

use crate::message::{Push, Request, RequestId, Response};

/// Builds an object with a `kind` discriminator.
fn tagged(kind: &str, id: RequestId) -> JsonMap<String, Json> {
    let mut m = JsonMap::new();
    m.insert("kind".to_string(), Json::String(kind.to_string()));
    // A decimal string: `u64` exceeds JavaScript's safe integer range.
    m.insert("id".to_string(), Json::String(id.0.to_string()));
    m
}

fn field<'a>(obj: &'a JsonMap<String, Json>, name: &str) -> Result<&'a Json, CodecError> {
    obj.get(name)
        .ok_or_else(|| CodecError::BadShape(format!("missing \"{name}\"")))
}

fn as_object<'a>(json: &'a Json, what: &str) -> Result<&'a JsonMap<String, Json>, CodecError> {
    json.as_object()
        .ok_or_else(|| CodecError::BadShape(format!("{what} must be an object")))
}

/// Reads one of the envelope's id fields, enforcing both halves of the wire's
/// id rule via the single definition in [`duet_codec::parse_wire_id`].
///
/// **Canonical spelling**: no leading `+`, no leading zeros, no surrounding
/// space. A bare `str::parse::<u64>()` would accept `"007"` and `"+1"`. That is
/// not cosmetic here: `tagged()` above re-encodes every id in canonical form,
/// so a guest that sent `"007"` would receive `"7"` back, never match it
/// against the pending entry it keyed by the string it sent, and hang forever
/// with no error.
///
/// **Domain `0..=`[`duet_codec::MAX_WIRE_ID`]** (`i64::MAX`): Dart's native
/// `int` is 64-bit *signed*, so an id above that bound is one no Dart guest can
/// parse at all. The Rust types stay `u64`; only this decoder narrows.
///
/// Gates every id field the envelope carries: `id` on requests and responses,
/// and `subscription` on both `unsubscribe` and `subscribed`.
fn u64_field(obj: &JsonMap<String, Json>, name: &str) -> Result<u64, CodecError> {
    let s = field(obj, name)?
        .as_str()
        .ok_or_else(|| CodecError::BadShape(format!("\"{name}\" must be a decimal string")))?;
    duet_codec::parse_wire_id(s).ok_or_else(|| CodecError::BadInt(format!("\"{name}\": {s}")))
}

fn kind(obj: &JsonMap<String, Json>) -> Result<&str, CodecError> {
    field(obj, "kind")?
        .as_str()
        .ok_or_else(|| CodecError::BadShape("\"kind\" must be a string".to_string()))
}

/// Encodes a request.
pub(crate) fn encode_request(request: &Request) -> Json {
    let m = match request {
        Request::Get { id, path } => {
            let mut m = tagged("get", *id);
            m.insert("path".to_string(), Json::String(path.to_string()));
            m
        }
        Request::Set { id, path, value } => {
            let mut m = tagged("set", *id);
            m.insert("path".to_string(), Json::String(path.to_string()));
            m.insert("value".to_string(), duet_codec::encode_value(value));
            m
        }
        Request::Subscribe { id, path } => {
            let mut m = tagged("subscribe", *id);
            m.insert("path".to_string(), Json::String(path.to_string()));
            m
        }
        Request::Unsubscribe { id, subscription } => {
            let mut m = tagged("unsubscribe", *id);
            m.insert(
                "subscription".to_string(),
                Json::String(subscription.0.to_string()),
            );
            m
        }
    };
    Json::Object(m)
}

/// Decodes a request.
///
/// # Errors
///
/// A [`CodecError`] describing the first structural problem found. Total over
/// all JSON input: never panics, whatever a guest sends.
pub(crate) fn decode_request(json: &Json) -> Result<Request, CodecError> {
    let obj = as_object(json, "request")?;
    let id = RequestId(u64_field(obj, "id")?);
    let path_of = |obj: &JsonMap<String, Json>| -> Result<duet_core::Path, CodecError> {
        let s = field(obj, "path")?
            .as_str()
            .ok_or_else(|| CodecError::BadShape("\"path\" must be a string".to_string()))?;
        duet_core::Path::parse(s).map_err(|e| CodecError::BadPath(e.to_string()))
    };

    match kind(obj)? {
        "get" => Ok(Request::Get {
            id,
            path: path_of(obj)?,
        }),
        "set" => Ok(Request::Set {
            id,
            path: path_of(obj)?,
            value: duet_codec::decode_value(field(obj, "value")?)?,
        }),
        "subscribe" => Ok(Request::Subscribe {
            id,
            path: path_of(obj)?,
        }),
        "unsubscribe" => Ok(Request::Unsubscribe {
            id,
            subscription: SubscriptionId(u64_field(obj, "subscription")?),
        }),
        other => Err(CodecError::UnknownTag(other.to_string())),
    }
}

/// Encodes a response.
pub(crate) fn encode_response(response: &Response) -> Json {
    let m = match response {
        Response::Value { id, value } => {
            let mut m = tagged("value", *id);
            m.insert(
                "value".to_string(),
                match value {
                    Some(v) => duet_codec::encode_value(v),
                    None => Json::Null,
                },
            );
            m
        }
        Response::Done { id } => tagged("done", *id),
        Response::Subscribed {
            id,
            subscription,
            snapshot,
        } => {
            let mut m = tagged("subscribed", *id);
            m.insert(
                "subscription".to_string(),
                Json::String(subscription.0.to_string()),
            );
            m.insert(
                "snapshot".to_string(),
                match snapshot {
                    Some(v) => duet_codec::encode_value(v),
                    None => Json::Null,
                },
            );
            m
        }
        Response::Failed { id, message } => {
            let mut m = tagged("failed", *id);
            m.insert("message".to_string(), Json::String(message.clone()));
            m
        }
    };
    Json::Object(m)
}

/// Decodes an optional value: JSON `null` means absent.
///
/// Distinct from `Value::Null`, which encodes as `{"t":"n"}` — so an absent
/// path and a path holding null stay distinguishable.
fn optional_value(json: &Json) -> Result<Option<duet_core::Value>, CodecError> {
    if json.is_null() {
        return Ok(None);
    }
    duet_codec::decode_value(json).map(Some)
}

/// Decodes a response.
///
/// # Errors
///
/// A [`CodecError`] describing the first structural problem found.
pub(crate) fn decode_response(json: &Json) -> Result<Response, CodecError> {
    let obj = as_object(json, "response")?;
    let id = RequestId(u64_field(obj, "id")?);

    match kind(obj)? {
        "value" => Ok(Response::Value {
            id,
            value: optional_value(field(obj, "value")?)?,
        }),
        "done" => Ok(Response::Done { id }),
        "subscribed" => Ok(Response::Subscribed {
            id,
            subscription: SubscriptionId(u64_field(obj, "subscription")?),
            snapshot: optional_value(field(obj, "snapshot")?)?,
        }),
        "failed" => Ok(Response::Failed {
            id,
            message: field(obj, "message")?
                .as_str()
                .ok_or_else(|| CodecError::BadShape("\"message\" must be a string".to_string()))?
                .to_string(),
        }),
        other => Err(CodecError::UnknownTag(other.to_string())),
    }
}

/// Encodes a push.
pub(crate) fn encode_push(push: &Push) -> Json {
    match push {
        Push::Notification(n) => {
            let mut m = JsonMap::new();
            m.insert("kind".to_string(), Json::String("notification".to_string()));
            m.insert(
                "notification".to_string(),
                duet_codec::encode_notification(n),
            );
            Json::Object(m)
        }
    }
}

/// Decodes a push.
///
/// # Errors
///
/// A [`CodecError`] describing the first structural problem found.
pub(crate) fn decode_push(json: &Json) -> Result<Push, CodecError> {
    let obj = as_object(json, "push")?;
    match kind(obj)? {
        "notification" => Ok(Push::Notification(duet_codec::decode_notification(field(
            obj,
            "notification",
        )?)?)),
        other => Err(CodecError::UnknownTag(other.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use duet_core::{Path, SubscriptionId, Value};

    fn json(s: &str) -> serde_json::Value {
        serde_json::from_str(s).expect("test JSON should parse")
    }

    fn p(s: &str) -> Path {
        Path::parse(s).expect("test path should parse")
    }

    #[test]
    fn a_get_request_encodes_with_its_kind_and_id() {
        assert_eq!(
            encode_request(&Request::Get {
                id: RequestId(7),
                path: p("editor.zoom")
            }),
            json(r#"{"kind":"get","id":"7","path":"editor.zoom"}"#)
        );
    }

    #[test]
    fn request_ids_travel_as_strings_so_both_guests_agree() {
        // The largest id the wire admits still exceeds JavaScript's safe
        // integer range (2^53) by five orders of magnitude, which is why ids
        // are strings rather than JSON numbers.
        let big = RequestId(duet_codec::MAX_WIRE_ID);
        let encoded = encode_request(&Request::Get {
            id: big,
            path: p("a"),
        });
        assert_eq!(encoded["id"], json(r#""9223372036854775807""#));
        assert_eq!(
            decode_request(&encoded).expect("decodes").id(),
            big,
            "the id must survive intact"
        );
    }

    #[test]
    fn every_request_variant_round_trips() {
        for original in [
            Request::Get {
                id: RequestId(1),
                path: p("a.b"),
            },
            Request::Set {
                id: RequestId(2),
                path: p("a[0]"),
                value: Value::Bytes(vec![1, 2, 3]),
            },
            Request::Subscribe {
                id: RequestId(3),
                path: Path::root(),
            },
            Request::Unsubscribe {
                id: RequestId(4),
                subscription: SubscriptionId(duet_codec::MAX_WIRE_ID),
            },
        ] {
            let decoded = decode_request(&encode_request(&original)).expect("decodes");
            assert_eq!(decoded, original);
        }
    }

    #[test]
    fn every_response_variant_round_trips() {
        for original in [
            Response::Value {
                id: RequestId(1),
                value: Some(Value::Float(1.5)),
            },
            Response::Value {
                id: RequestId(2),
                value: None,
            },
            Response::Done { id: RequestId(3) },
            Response::Subscribed {
                id: RequestId(4),
                subscription: SubscriptionId(9),
                snapshot: Some(Value::Str("x".into())),
            },
            Response::Failed {
                id: RequestId(5),
                message: "boom".to_string(),
            },
        ] {
            let decoded = decode_response(&encode_response(&original)).expect("decodes");
            assert_eq!(decoded, original);
        }
    }

    #[test]
    fn a_push_round_trips() {
        let original = Push::Notification(duet_core::Notification {
            subscriber: duet_core::SubscriberId(1),
            subscription: SubscriptionId(2),
            patch: duet_core::Patch {
                path: p("a"),
                value: Value::Bool(true),
            },
        });
        assert_eq!(
            decode_push(&encode_push(&original)).expect("decodes"),
            original
        );
    }

    #[test]
    fn non_canonical_ids_are_rejected() {
        // "7" and "007" must not both decode to 7: the host echoes the CANONICAL
        // form back, so a guest keying its pending map by the string it sent
        // would wait forever for a reply it already received. duet-codec's
        // canonical.rs states this as a property of the format; duet-protocol
        // must enforce it too.
        for raw in [
            "007",
            "+1",
            "0000000000000000000007",
            "1_000",
            " 1",
            "1 ",
            "",
        ] {
            let text = format!(r#"{{"kind":"get","id":"{raw}","path":"a"}}"#);
            let json: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
            assert!(
                decode_request(&json).is_err(),
                "id {raw:?} must be rejected as non-canonical"
            );
        }
    }

    #[test]
    fn canonical_ids_are_still_accepted() {
        // The positive control: the fix must not reject what it should accept,
        // including 0 and a value above 2^53 (the reason ids are strings at all).
        // The top of the list is i64::MAX, not u64::MAX — see
        // `the_wire_id_domain_stops_at_i64_max`.
        for raw in ["0", "1", "7", "9007199254740993", "9223372036854775807"] {
            let text = format!(r#"{{"kind":"get","id":"{raw}","path":"a"}}"#);
            let json: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
            assert!(decode_request(&json).is_ok(), "id {raw:?} must be accepted");
        }
    }

    #[test]
    fn non_canonical_subscription_ids_are_rejected() {
        // `subscription` is the second u64 a guest sends, and it is echoed on
        // the `subscribed` reply exactly as `id` is. Same hazard, same rule.
        for raw in ["007", "+1", "0000000000000000000007", " 1", ""] {
            let text = format!(r#"{{"kind":"unsubscribe","id":"1","subscription":"{raw}"}}"#);
            let json: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
            assert!(
                decode_request(&json).is_err(),
                "subscription {raw:?} must be rejected as non-canonical"
            );
        }
        for raw in ["0", "1", "9007199254740993", "9223372036854775807"] {
            let text = format!(r#"{{"kind":"unsubscribe","id":"1","subscription":"{raw}"}}"#);
            let json: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
            assert!(
                decode_request(&json).is_ok(),
                "subscription {raw:?} must be accepted"
            );
        }
    }

    #[test]
    fn non_canonical_response_ids_are_rejected() {
        // The guest side of the same rule: a guest decoding a host reply must
        // refuse an id it could not have sent, rather than silently matching
        // it against a differently-spelled pending entry.
        for raw in ["007", "+1", " 1", ""] {
            let text = format!(r#"{{"kind":"done","id":"{raw}"}}"#);
            let json: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
            assert!(
                decode_response(&json).is_err(),
                "response id {raw:?} must be rejected as non-canonical"
            );
        }
    }

    #[test]
    fn the_wire_id_domain_stops_at_i64_max() {
        // Dart's native int is 64-bit SIGNED: int.tryParse("9223372036854775808")
        // returns null, so a Dart guest cannot read an id the host could emit.
        // Narrowing here costs nothing (ids are sequential from 1) and makes one
        // id domain hold in every guest language.
        for raw in ["0", "1", "9007199254740993", "9223372036854775807"] {
            let text = format!(r#"{{"kind":"get","id":"{raw}","path":"a"}}"#);
            let json: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
            assert!(
                decode_request(&json).is_ok(),
                "id {raw:?} is within the domain"
            );
        }
        for raw in [
            "9223372036854775808",
            "18446744073709551615",
            "99999999999999999999",
        ] {
            let text = format!(r#"{{"kind":"get","id":"{raw}","path":"a"}}"#);
            let json: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
            assert!(
                decode_request(&json).is_err(),
                "id {raw:?} is outside the domain"
            );
        }
    }

    #[test]
    fn the_id_domain_binds_subscription_and_response_ids_too() {
        // `id` is only one of the envelope's u64 fields. `subscription` travels
        // on `unsubscribe` and is echoed on `subscribed`; a response `id` is
        // read by the guest. All of them must share one domain, or a guest
        // would accept an id in one field that it rejects in another.
        for raw in ["9223372036854775808", "18446744073709551615"] {
            let unsubscribe =
                format!(r#"{{"kind":"unsubscribe","id":"1","subscription":"{raw}"}}"#);
            assert!(
                decode_request(&json(&unsubscribe)).is_err(),
                "unsubscribe subscription {raw:?} is outside the domain"
            );

            let done = format!(r#"{{"kind":"done","id":"{raw}"}}"#);
            assert!(
                decode_response(&json(&done)).is_err(),
                "response id {raw:?} is outside the domain"
            );

            let subscribed = format!(
                r#"{{"kind":"subscribed","id":"1","subscription":"{raw}","snapshot":null}}"#
            );
            assert!(
                decode_response(&json(&subscribed)).is_err(),
                "subscribed subscription {raw:?} is outside the domain"
            );
        }
        // The positive control at the exact boundary, in every one of those
        // three fields: the domain must stop AT i64::MAX, not below it.
        assert!(
            decode_request(&json(
                r#"{"kind":"unsubscribe","id":"1","subscription":"9223372036854775807"}"#
            ))
            .is_ok()
        );
        assert!(decode_response(&json(r#"{"kind":"done","id":"9223372036854775807"}"#)).is_ok());
        assert!(
            decode_response(&json(
                r#"{"kind":"subscribed","id":"1","subscription":"9223372036854775807","snapshot":null}"#
            ))
            .is_ok()
        );
    }

    #[test]
    fn an_id_above_the_domain_is_encoded_faithfully_and_then_refused() {
        // The wire types stay `u64`, so a host CAN construct an id outside the
        // domain. The encoder is deliberately neither lossy nor panicky: it
        // emits the id verbatim. Clamping would answer a different request than
        // the one asked; panicking would take the host down on a guest-reachable
        // path. Instead the invariant ("ids are 0..=MAX_WIRE_ID") is documented
        // on `RequestId`, and every decoder — including Rust's own, here —
        // refuses the value, so an encoder that ever violated it fails loudly at
        // the first hop rather than shipping an id only some guests can read.
        let out_of_domain = RequestId(u64::MAX);
        let encoded = encode_request(&Request::Get {
            id: out_of_domain,
            path: p("a"),
        });
        assert_eq!(
            encoded["id"],
            json(r#""18446744073709551615""#),
            "the encoder must emit the id verbatim, never clamp it"
        );
        assert!(
            decode_request(&encoded).is_err(),
            "an out-of-domain id must not round-trip"
        );
    }

    #[test]
    fn decode_rejects_malformed_messages_without_panicking() {
        for bad in [
            r#"42"#,
            r#"{}"#,
            r#"{"kind":"nope","id":"1"}"#,
            r#"{"kind":"get"}"#,
            r#"{"kind":"get","id":1,"path":"a"}"#,
            r#"{"kind":"get","id":"x","path":"a"}"#,
            // Historical gap: this list tested a numeric id and "x" but never
            // "007", which is why non-canonical ids survived here for so long
            // while `duet-codec` and the Dart client both already rejected
            // them. `non_canonical_ids_are_rejected` above is the real
            // coverage; this line pins the specific case that was missing.
            r#"{"kind":"get","id":"007","path":"a"}"#,
            r#"{"kind":"get","id":"1","path":"a.[0]"}"#,
            r#"{"kind":"set","id":"1","path":"a"}"#,
            r#"{"kind":"unsubscribe","id":"1","subscription":"x"}"#,
        ] {
            let parsed = json(bad);
            assert!(
                decode_request(&parsed).is_err(),
                "{bad} must be rejected, got {:?}",
                decode_request(&parsed)
            );
        }
    }

    #[test]
    fn guest_supplied_text_is_bounded_in_error_messages() {
        // This decodes untrusted input; an unbounded echo turns a 1 MB payload
        // into a 1 MB log line.
        let huge = "z".repeat(10_000);
        let bad = json(&format!(r#"{{"kind":"{huge}","id":"1"}}"#));
        let rendered = decode_request(&bad).expect_err("must reject").to_string();
        assert!(
            rendered.len() < 300,
            "error message must be bounded, got {} chars",
            rendered.len()
        );
    }
}
