//! Encoding for the addressed types: `Path`, `Patch`, `Notification`.

use duet_core::{Notification, Patch, Path, SubscriberId, SubscriptionId};
use serde_json::{Map as JsonMap, Value as Json};

use crate::canonical::is_canonical_unsigned_digits;
use crate::error::CodecError;
use crate::value::{decode_value, encode_value};

/// Encodes a [`Path`] as its `Display` string.
///
/// `duet-core` proves by exhaustive property test that `Path::parse` and
/// `Display` are mutually inverse, so this reuses a guarantee that is already
/// pinned rather than introducing a second representation to keep in sync.
pub(crate) fn encode_path(path: &Path) -> Json {
    Json::String(path.to_string())
}

/// Decodes a path string.
///
/// # Errors
///
/// [`CodecError::BadShape`] if the JSON is not a string, or
/// [`CodecError::BadPath`] carrying the rendered parse error — whose byte
/// offsets are more actionable for a guest than a bare failure.
pub(crate) fn decode_path(json: &Json) -> Result<Path, CodecError> {
    let s = json
        .as_str()
        .ok_or_else(|| CodecError::BadShape("path must be a string".to_string()))?;
    Path::parse(s).map_err(|e| CodecError::BadPath(e.to_string()))
}

/// Reads a required field.
fn field<'a>(obj: &'a JsonMap<String, Json>, name: &str) -> Result<&'a Json, CodecError> {
    obj.get(name)
        .ok_or_else(|| CodecError::BadShape(format!("missing \"{name}\"")))
}

/// Reads a `u64` carried as a canonical decimal string: no leading `+` and no
/// leading zeros. Without this, `"7"` and `"007"` would decode to the same
/// id and both re-encode as `"7"` — the same non-canonical-input hazard the
/// base64 decoder and `duet-core`'s path parser (`[007]`) already reject.
fn u64_field(obj: &JsonMap<String, Json>, name: &str) -> Result<u64, CodecError> {
    let s = field(obj, name)?
        .as_str()
        .ok_or_else(|| CodecError::BadShape(format!("\"{name}\" must be a decimal string")))?;
    if !is_canonical_unsigned_digits(s) {
        return Err(CodecError::BadInt(format!("\"{name}\": {s}")));
    }
    s.parse::<u64>()
        .map_err(|_| CodecError::BadInt(format!("\"{name}\": {s}")))
}

fn as_object<'a>(json: &'a Json, what: &str) -> Result<&'a JsonMap<String, Json>, CodecError> {
    json.as_object()
        .ok_or_else(|| CodecError::BadShape(format!("{what} must be an object")))
}

/// Encodes a [`Patch`].
pub(crate) fn encode_patch(patch: &Patch) -> Json {
    let mut m = JsonMap::new();
    m.insert("path".to_string(), encode_path(&patch.path));
    m.insert("value".to_string(), encode_value(&patch.value));
    Json::Object(m)
}

/// Decodes a [`Patch`].
///
/// # Errors
///
/// A [`CodecError`] describing the first structural problem found.
pub(crate) fn decode_patch(json: &Json) -> Result<Patch, CodecError> {
    let obj = as_object(json, "patch")?;
    Ok(Patch {
        path: decode_path(field(obj, "path")?)?,
        value: decode_value(field(obj, "value")?)?,
    })
}

/// Encodes a [`Notification`].
///
/// Both ids travel as decimal strings: `u64` exceeds JavaScript's safe integer
/// range just as `i64` does, and an id that differs between the two guests
/// would misroute notifications.
pub(crate) fn encode_notification(note: &Notification) -> Json {
    let mut m = JsonMap::new();
    m.insert(
        "subscriber".to_string(),
        Json::String(note.subscriber.0.to_string()),
    );
    m.insert(
        "subscription".to_string(),
        Json::String(note.subscription.0.to_string()),
    );
    m.insert("patch".to_string(), encode_patch(&note.patch));
    Json::Object(m)
}

/// Decodes a [`Notification`].
///
/// # Errors
///
/// A [`CodecError`] describing the first structural problem found.
pub(crate) fn decode_notification(json: &Json) -> Result<Notification, CodecError> {
    let obj = as_object(json, "notification")?;
    Ok(Notification {
        subscriber: SubscriberId(u64_field(obj, "subscriber")?),
        subscription: SubscriptionId(u64_field(obj, "subscription")?),
        patch: decode_patch(field(obj, "patch")?)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use duet_core::{Notification, Patch, Path, SubscriberId, SubscriptionId, Value};

    fn json(s: &str) -> serde_json::Value {
        serde_json::from_str(s).expect("test JSON should parse")
    }

    fn p(s: &str) -> Path {
        Path::parse(s).expect("test path should parse")
    }

    #[test]
    fn path_encodes_as_its_display_string() {
        // Reuses duet-core's proven parse/Display inverse rather than
        // inventing a second representation.
        assert_eq!(encode_path(&p("editor.zoom")), json(r#""editor.zoom""#));
        assert_eq!(
            encode_path(&p("documents[3].title")),
            json(r#""documents[3].title""#)
        );
        assert_eq!(encode_path(&Path::root()), json(r#""""#));
    }

    #[test]
    fn path_round_trips_including_root_and_indices() {
        for raw in [
            "",
            "editor.zoom",
            "documents[3].title",
            "a[0][1].b",
            "café.zoom",
        ] {
            let path = p(raw);
            let decoded = decode_path(&encode_path(&path)).expect("decodes");
            assert_eq!(decoded, path, "round trip failed for {raw:?}");
        }
    }

    #[test]
    fn decode_path_rejects_malformed_strings() {
        for bad in [
            r#""foo]""#,
            r#""a.[0]""#,
            r#""a[007]""#,
            r#""foo[""#,
            r#"42"#,
        ] {
            assert!(
                decode_path(&json(bad)).is_err(),
                "{bad} must be rejected, got {:?}",
                decode_path(&json(bad))
            );
        }
    }

    #[test]
    fn patch_carries_path_and_value() {
        let patch = Patch {
            path: p("editor.zoom"),
            value: Value::Float(1.5),
        };
        assert_eq!(
            encode_patch(&patch),
            json(r#"{"path":"editor.zoom","value":{"t":"f","v":1.5}}"#)
        );
        assert_eq!(decode_patch(&encode_patch(&patch)).expect("decodes"), patch);
    }

    #[test]
    fn notification_carries_both_ids_as_strings() {
        // u64 ids exceed JavaScript's safe integer range just as i64 does, so
        // they travel as strings for the same reason.
        let note = Notification {
            subscriber: SubscriberId(u64::MAX),
            subscription: SubscriptionId(7),
            patch: Patch {
                path: p("a"),
                value: Value::Null,
            },
        };
        let encoded = encode_notification(&note);
        assert_eq!(
            encoded,
            json(
                r#"{"subscriber":"18446744073709551615","subscription":"7",
                    "patch":{"path":"a","value":{"t":"n"}}}"#
            )
        );
        assert_eq!(decode_notification(&encoded).expect("decodes"), note);
    }

    #[test]
    fn decode_rejects_malformed_patches_and_notifications() {
        for bad in [
            r#"{}"#,
            r#"{"path":"a"}"#,
            r#"{"value":{"t":"n"}}"#,
            r#"[]"#,
        ] {
            assert!(decode_patch(&json(bad)).is_err(), "{bad} must be rejected");
        }
        for bad in [
            r#"{}"#,
            r#"{"subscriber":"1"}"#,
            r#"{"subscriber":1,"subscription":"1","patch":{"path":"a","value":{"t":"n"}}}"#,
            r#"{"subscriber":"x","subscription":"1","patch":{"path":"a","value":{"t":"n"}}}"#,
            // non-canonical ids: same hazard as non-canonical Int payloads.
            r#"{"subscriber":"+1","subscription":"1","patch":{"path":"a","value":{"t":"n"}}}"#,
            r#"{"subscriber":"007","subscription":"1","patch":{"path":"a","value":{"t":"n"}}}"#,
        ] {
            assert!(
                decode_notification(&json(bad)).is_err(),
                "{bad} must be rejected"
            );
        }
    }

    #[test]
    fn canonical_zero_id_still_decodes() {
        let note = Notification {
            subscriber: SubscriberId(0),
            subscription: SubscriptionId(0),
            patch: Patch {
                path: p("a"),
                value: Value::Null,
            },
        };
        let encoded = encode_notification(&note);
        assert_eq!(decode_notification(&encoded).expect("decodes"), note);
    }
}
