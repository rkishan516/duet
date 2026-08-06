//! Why a [`Value`] could not be read as a Rust type.

use duet_core::{Segment, Value};

use crate::echo;

/// A decode that could not produce the requested type.
///
/// # Not an exception, and never a panic
///
/// Every [`SharedState::from_value`](crate::SharedState::from_value)
/// implementation is **total** over `Value`. That is not a nicety: a guest can
/// write any value to any path — the two-guest proof has a webview and a
/// Flutter engine writing one store at the same time — so a typed read *will*
/// eventually meet a value it cannot decode. The honest report of that is data
/// the caller inspects ([`Reading::Mismatch`](crate::Reading::Mismatch)), not a
/// crash on the host thread that owns the store.
///
/// # What it carries, and what it deliberately does not
///
/// [`location`](Self::location) says *where inside the value* the problem was,
/// building up as the error propagates outward from the leaf that refused. It
/// does **not** carry the offending `Value` itself: the caller already has it
/// (`Reading::Mismatch` keeps it alongside this error), and copying an
/// arbitrarily large guest-written subtree into every error would make the
/// error's size a function of input the host does not control.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodeError {
    reason: DecodeReason,
    /// Innermost segment first while the error travels outward; reversed once,
    /// lazily, by [`location`](Self::location) and [`Display`].
    reversed: Vec<Segment>,
}

/// The specific thing that was wrong, without the location.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum DecodeReason {
    /// The value was the wrong kind of node entirely.
    WrongType {
        /// The Rust type that was being decoded, e.g. `"i64"` or `"Editor"`.
        expected: &'static str,
        /// The [`Value`] variant actually found, e.g. `"string"`. A variant
        /// name, never the value's contents, so this can never echo guest data.
        found: &'static str,
    },
    /// The value was an integer, and it did not fit the target type.
    ///
    /// Reachable for every integer narrower than `i64`: the wire has exactly
    /// one integer type, so a guest may write `300` to a path whose schema
    /// says `u8`.
    OutOfRange {
        /// The Rust type that was being decoded, e.g. `"u8"`.
        expected: &'static str,
        /// The integer found. Bounded by construction — it is an `i64`, not
        /// guest-chosen text — so it is rendered in full.
        found: i64,
    },
    /// A map key the type requires was not present.
    ///
    /// The key is a `&'static str` written by the host (a struct field name),
    /// never a guest-chosen string, so it is rendered in full.
    MissingField {
        /// The Rust type that was being decoded.
        expected: &'static str,
        /// The absent key.
        key: &'static str,
    },
}

impl DecodeError {
    /// The value was the wrong kind of node for `expected`.
    pub fn wrong_type(expected: &'static str, found: &Value) -> DecodeError {
        DecodeError {
            reason: DecodeReason::WrongType {
                expected,
                found: variant_name(found),
            },
            reversed: Vec::new(),
        }
    }

    /// An integer was found, and it does not fit `expected`.
    pub fn out_of_range(expected: &'static str, found: i64) -> DecodeError {
        DecodeError {
            reason: DecodeReason::OutOfRange { expected, found },
            reversed: Vec::new(),
        }
    }

    /// A map key `expected` requires was absent.
    pub fn missing_field(expected: &'static str, key: &'static str) -> DecodeError {
        DecodeError {
            reason: DecodeReason::MissingField { expected, key },
            reversed: Vec::new(),
        }
    }

    /// Records that this error came from inside the map key `key`.
    ///
    /// Called as the error propagates *outward*, so callers push the innermost
    /// segment first and never have to reverse anything themselves. Takes and
    /// returns `self` so a decode body can write
    /// `T::from_value(v).map_err(|e| e.within_key(k))`.
    #[must_use]
    pub fn within_key(mut self, key: impl Into<String>) -> DecodeError {
        self.reversed.push(Segment::Key(key.into()));
        self
    }

    /// Records that this error came from inside list element `index`.
    #[must_use]
    pub fn within_index(mut self, index: usize) -> DecodeError {
        self.reversed.push(Segment::Index(index));
        self
    }

    /// What was wrong, without the location.
    pub fn reason(&self) -> &DecodeReason {
        &self.reason
    }

    /// Where inside the value the problem was, outermost segment first.
    ///
    /// The root of the decoded value is the empty slice — the value itself was
    /// the wrong kind of node, rather than something nested inside it.
    pub fn location(&self) -> Vec<Segment> {
        self.reversed.iter().rev().cloned().collect()
    }
}

/// The name of a [`Value`]'s variant, for error messages.
///
/// These are the *schema* spellings, not the Rust variant names, because they
/// are what a developer reading a mismatch sees in the generated Dart and
/// TypeScript too — one vocabulary across all three languages.
fn variant_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Int(_) => "int",
        Value::Float(_) => "float",
        Value::Str(_) => "string",
        Value::Bytes(_) => "bytes",
        Value::List(_) => "list",
        Value::Map(_) => "map",
    }
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let at = echo::location(&self.location());
        match &self.reason {
            DecodeReason::WrongType { expected, found } => {
                write!(f, "expected {expected} at {at}, found {found}")
            }
            DecodeReason::OutOfRange { expected, found } => {
                write!(f, "{found} at {at} does not fit {expected}")
            }
            DecodeReason::MissingField { expected, key } => {
                write!(f, "{expected} at {at} is missing the key \"{key}\"")
            }
        }
    }
}

impl std::error::Error for DecodeError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wrong_type_names_both_sides() {
        let error = DecodeError::wrong_type("i64", &Value::Str("hi".into()));
        assert_eq!(error.to_string(), "expected i64 at <root>, found string");
    }

    #[test]
    fn every_value_variant_has_a_schema_spelling() {
        let cases = [
            (Value::Null, "null"),
            (Value::Bool(true), "bool"),
            (Value::Int(1), "int"),
            (Value::Float(1.0), "float"),
            (Value::Str(String::new()), "string"),
            (Value::Bytes(Vec::new()), "bytes"),
            (Value::List(Vec::new()), "list"),
            (Value::map([]), "map"),
        ];
        for (value, expected) in cases {
            assert_eq!(variant_name(&value), expected, "for {value:?}");
        }
    }

    #[test]
    fn location_builds_outward_and_reads_inward() {
        // Pushed innermost-first, exactly as a decode body unwinds.
        let error = DecodeError::wrong_type("i64", &Value::Null)
            .within_key("title")
            .within_index(3)
            .within_key("documents");
        assert_eq!(
            error.location(),
            vec![
                Segment::Key("documents".to_string()),
                Segment::Index(3),
                Segment::Key("title".to_string()),
            ]
        );
        assert_eq!(
            error.to_string(),
            "expected i64 at documents[3].title, found null"
        );
    }

    #[test]
    fn an_out_of_range_integer_reports_the_integer() {
        let error = DecodeError::out_of_range("u8", 300).within_key("count");
        assert_eq!(error.to_string(), "300 at count does not fit u8");
    }

    #[test]
    fn a_missing_field_names_the_key() {
        let error = DecodeError::missing_field("Editor", "zoom");
        assert_eq!(
            error.to_string(),
            "Editor at <root> is missing the key \"zoom\""
        );
    }

    #[test]
    fn a_guest_chosen_map_key_cannot_grow_the_message() {
        // The location is the one part of a decode error a guest influences.
        let error = DecodeError::wrong_type("i64", &Value::Null).within_key("k".repeat(100_000));
        assert!(
            error.to_string().len() < 256,
            "got {} bytes",
            error.to_string().len()
        );
        // The field itself is not truncated: a host handler matching on the
        // error still sees the whole key.
        assert_eq!(error.location().len(), 1);
        match &error.location()[0] {
            Segment::Key(key) => assert_eq!(key.len(), 100_000),
            other => panic!("expected a key segment, got {other:?}"),
        }
    }

    #[test]
    fn the_reason_is_readable_without_the_location() {
        let error = DecodeError::out_of_range("u32", -1);
        assert_eq!(
            error.reason(),
            &DecodeReason::OutOfRange {
                expected: "u32",
                found: -1
            }
        );
    }

    #[test]
    fn decode_error_implements_std_error() {
        let error: &dyn std::error::Error = &DecodeError::wrong_type("i64", &Value::Null);
        assert!(error.to_string().contains("i64"));
    }
}
