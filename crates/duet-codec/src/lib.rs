//! Wire format for Duet.
//!
//! Encodes the types that cross the IPC boundary between the Rust host and its
//! Flutter and JavaScript guests. **This crate decodes untrusted input** — every
//! decode path is total: malformed bytes produce a [`CodecError`], never a panic.
//!
//! # Why the encoding is tagged
//!
//! Every value encodes as `{"t":"<tag>","v":<payload>}`. Plain JSON cannot
//! represent [`duet_core::Value`] faithfully: `Bytes` and `Str` would collapse
//! into one JSON string, `Int(1)` and `Float(1.0)` would both become `1`, and
//! `NaN` has no JSON form at all — it would decode back as `Null`, changing the
//! *variant* rather than the magnitude.
//!
//! `Int` is carried as a **string**, not a JSON number, because JavaScript
//! numbers are IEEE-754 doubles: an `i64` above 2^53 would lose precision in the
//! webview while surviving intact in Dart. Two guests disagreeing about a value
//! is the worst kind of bug this format could ship.
//!
//! Verbosity is an accepted cost. Payloads are small patches, guests never see
//! the wire format directly (Phase 4 generates typed accessors over it), and a
//! compact binary encoding could replace this one later without changing the
//! free-function API below — callers depend on functions, not a concrete
//! codec type, so nothing here commits to a trait that does not exist yet.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

mod base64;
pub mod canonical;
pub mod error;
mod value;
mod wire;

pub use canonical::{
    MAX_WIRE_ID, is_canonical_signed_decimal, is_canonical_unsigned_digits, parse_wire_id,
};
pub use error::CodecError;

/// Encodes a [`duet_core::Value`] into its tagged JSON representation.
pub fn encode_value(value: &duet_core::Value) -> serde_json::Value {
    value::encode_value(value)
}

/// Decodes a tagged JSON representation back into a [`duet_core::Value`].
///
/// # Errors
///
/// Returns a [`CodecError`] describing the first structural problem found. This
/// function is total over all JSON input: it never panics, whatever a guest
/// sends.
pub fn decode_value(json: &serde_json::Value) -> Result<duet_core::Value, CodecError> {
    value::decode_value(json)
}

/// Encodes a [`duet_core::Patch`].
pub fn encode_patch(patch: &duet_core::Patch) -> serde_json::Value {
    wire::encode_patch(patch)
}

/// Decodes a [`duet_core::Patch`].
///
/// # Errors
///
/// A [`CodecError`] describing the first structural problem found.
pub fn decode_patch(json: &serde_json::Value) -> Result<duet_core::Patch, CodecError> {
    wire::decode_patch(json)
}

/// Encodes a [`duet_core::Notification`].
pub fn encode_notification(note: &duet_core::Notification) -> serde_json::Value {
    wire::encode_notification(note)
}

/// Decodes a [`duet_core::Notification`].
///
/// # Errors
///
/// A [`CodecError`] describing the first structural problem found.
pub fn decode_notification(
    json: &serde_json::Value,
) -> Result<duet_core::Notification, CodecError> {
    wire::decode_notification(json)
}

#[cfg(test)]
mod tests {
    //! `value.rs` and `wire.rs` unit-test the encoding logic directly through
    //! their `pub(crate)` functions. These tests exist only to prove the thin
    //! public wrappers above actually delegate to them — every crate consumer
    //! goes through this module, not through `value`/`wire` directly.
    use super::*;
    use duet_core::{Notification, Patch, Path, SubscriberId, SubscriptionId, Value};

    #[test]
    fn public_patch_api_round_trips() {
        let patch = Patch {
            path: Path::parse("editor.zoom").expect("valid path"),
            value: Value::Int(3),
        };
        let encoded = encode_patch(&patch);
        assert_eq!(decode_patch(&encoded).expect("decodes"), patch);
    }

    #[test]
    fn public_notification_api_round_trips() {
        let note = Notification {
            subscriber: SubscriberId(1),
            subscription: SubscriptionId(2),
            patch: Patch {
                path: Path::root(),
                value: Value::Bool(true),
            },
        };
        let encoded = encode_notification(&note);
        assert_eq!(decode_notification(&encoded).expect("decodes"), note);
    }

    #[test]
    fn public_decode_functions_surface_codec_errors() {
        let bad = serde_json::json!({});
        assert!(matches!(decode_patch(&bad), Err(CodecError::BadShape(_))));
        assert!(matches!(
            decode_notification(&bad),
            Err(CodecError::BadShape(_))
        ));
    }
}
