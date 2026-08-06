//! What a typed field currently holds — including the answers that are not a
//! value.

use duet_core::Value;

use crate::decode::DecodeError;
use crate::state::{NotNullable, SharedState};

/// One typed observation of one path.
///
/// # Why a result type and not an error
///
/// A type mismatch is a **state the host can be in**, not a failure of the call
/// that found it. Another guest can write any value to any path — the two-guest
/// proof has a webview and a Flutter engine writing one store at the same time
/// — so a typed read will eventually meet a value its type refuses. That is
/// data. `Field::get` still returns a `Result`, but its `Err` is reserved for
/// the store being unreachable, which is a genuinely different condition and
/// one the application cannot inspect its way out of.
///
/// # Why four arms
///
/// Because the host can be in exactly four states and the wire already spends a
/// distinct spelling on each: a decodable value, `Value::Null`, no node at all,
/// and a value of some other type. Collapsing any two would delete a
/// distinction the protocol pays for.
///
/// These are the same four the Dart and TypeScript clients settled on
/// (`DuetPresent` / `DuetNone` / `DuetAbsent` / `DuetMismatch`), deliberately:
/// the whole point of the schema is that all three languages agree about what a
/// path holds, and they cannot agree if they disagree about what "empty" means.
#[derive(Debug, Clone, PartialEq)]
pub enum Reading<T> {
    /// The path holds a value the type accepted.
    Present(T),
    /// The path exists and holds [`Value::Null`] — Rust's `Option::None`.
    ///
    /// Only an [`OptionalField`](crate::OptionalField) produces this. A
    /// [`Field`](crate::Field) whose schema promises a `T` reports
    /// [`Reading::Mismatch`] for a null, because a null is not the `T` it was
    /// promised.
    None,
    /// There is no node at the path at all.
    ///
    /// Distinct from [`Reading::None`]: nothing was ever written here, or an
    /// ancestor of this path is itself `Null` or a scalar, or a list index is
    /// out of range.
    Absent,
    /// The path holds a value, and the type refused it.
    ///
    /// The honest report of two guests disagreeing about one path. Carries what
    /// was actually found, so an application can log it, show it, or write the
    /// right type back over it.
    Mismatch {
        /// The value the store actually holds at the path.
        found: Value,
        /// Why it was refused.
        error: DecodeError,
    },
}

impl<T> Reading<T> {
    /// The value if this is [`Reading::Present`], `None` otherwise.
    ///
    /// A convenience for the common "render it or render nothing" case. Prefer
    /// an exhaustive `match` wherever the three non-value outcomes differ —
    /// treating a mismatch as an absence is how a type disagreement between two
    /// guests becomes invisible.
    pub fn present(self) -> Option<T> {
        match self {
            Reading::Present(value) => Some(value),
            _ => None,
        }
    }

    /// True for [`Reading::Present`].
    pub fn is_present(&self) -> bool {
        matches!(self, Reading::Present(_))
    }

    /// True for [`Reading::Mismatch`].
    pub fn is_mismatch(&self) -> bool {
        matches!(self, Reading::Mismatch { .. })
    }
}

/// Reads `value` as a required `T`.
///
/// - `None` — no node at the path — is [`Reading::Absent`].
/// - Anything `T` accepts is [`Reading::Present`].
/// - Everything else, **including [`Value::Null`]**, is [`Reading::Mismatch`]:
///   a required field promised a `T`, and a null is not one.
pub fn required_reading<T: SharedState>(value: Option<Value>) -> Reading<T> {
    let Some(found) = value else {
        return Reading::Absent;
    };
    decode_or_mismatch(found)
}

/// Reads `value` as an optional `T`, mirroring Rust's `Option<T>`.
///
/// - `None` — no node at the path — is [`Reading::Absent`].
/// - [`Value::Null`] is [`Reading::None`], **without consulting `T`**:
///   `Option::None` lowers to `Value::Null` by definition, and the
///   [`NotNullable`] bound guarantees no `T` here may claim it.
/// - Anything else `T` accepts is [`Reading::Present`]; the rest is a mismatch.
pub fn optional_reading<T: SharedState + NotNullable>(value: Option<Value>) -> Reading<T> {
    match value {
        None => Reading::Absent,
        Some(Value::Null) => Reading::None,
        Some(found) => decode_or_mismatch(found),
    }
}

/// Runs `T`'s decoder over `found`, turning either outcome into a reading.
///
/// No `catch` equivalent, and none is needed: unlike the Dart and TypeScript
/// clients — which must tolerate an application-supplied codec that throws —
/// `SharedState::from_value` returns a `Result` and this crate's own impls are
/// total by construction. A hand-written impl that panics takes the process
/// down, which is the correct outcome for a bug in a decoder rather than
/// something to paper over.
fn decode_or_mismatch<T: SharedState>(found: Value) -> Reading<T> {
    match T::from_value(&found) {
        Ok(value) => Reading::Present(value),
        Err(error) => Reading::Mismatch { found, error },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_node_is_absent_for_both_kinds_of_field() {
        assert_eq!(required_reading::<i64>(None), Reading::Absent);
        assert_eq!(optional_reading::<i64>(None), Reading::Absent);
    }

    #[test]
    fn a_decodable_value_is_present() {
        assert_eq!(
            required_reading::<i64>(Some(Value::Int(7))),
            Reading::Present(7)
        );
        assert_eq!(
            optional_reading::<i64>(Some(Value::Int(7))),
            Reading::Present(7)
        );
    }

    #[test]
    fn null_splits_the_two_readings() {
        // The distinction the whole typed layer exists to keep: a required
        // field promised a value, so a null is a disagreement; an optional
        // field promised `Option`, so a null is the answer.
        assert!(required_reading::<i64>(Some(Value::Null)).is_mismatch());
        assert_eq!(optional_reading::<i64>(Some(Value::Null)), Reading::None);
    }

    #[test]
    fn a_wrong_typed_value_is_a_mismatch_carrying_what_was_found() {
        let reading = required_reading::<i64>(Some(Value::Str("nope".into())));
        match reading {
            Reading::Mismatch { found, error } => {
                assert_eq!(found, Value::Str("nope".into()));
                assert_eq!(error.to_string(), "expected i64 at <root>, found string");
            }
            other => panic!("expected a mismatch, got {other:?}"),
        }
    }

    #[test]
    fn absent_and_none_are_not_equal() {
        // Pinned because collapsing them is the single most tempting
        // simplification here, and it would silently delete the distinction
        // the wire pays a spelling for.
        assert_ne!(Reading::<i64>::Absent, Reading::None);
    }

    #[test]
    fn present_extracts_only_the_value_arm() {
        assert_eq!(Reading::Present(7i64).present(), Some(7));
        assert_eq!(Reading::<i64>::None.present(), None);
        assert_eq!(Reading::<i64>::Absent.present(), None);
        assert_eq!(
            Reading::<i64>::Mismatch {
                found: Value::Null,
                error: DecodeError::wrong_type("i64", &Value::Null),
            }
            .present(),
            None
        );
    }

    #[test]
    fn the_predicates_agree_with_the_arms() {
        assert!(Reading::Present(1i64).is_present());
        assert!(!Reading::<i64>::Absent.is_present());
        assert!(!Reading::<i64>::None.is_present());
        assert!(!Reading::Present(1i64).is_mismatch());
        assert!(
            Reading::<i64>::Mismatch {
                found: Value::Null,
                error: DecodeError::wrong_type("i64", &Value::Null),
            }
            .is_mismatch()
        );
    }

    #[test]
    fn a_struct_shaped_reading_survives_every_hostile_variant() {
        for value in [
            Value::Null,
            Value::Bool(true),
            Value::Int(1),
            Value::Float(1.0),
            Value::Str("x".into()),
            Value::Bytes(vec![1]),
            Value::List(vec![Value::Null]),
            Value::map([("a", Value::Null)]),
        ] {
            let _ = required_reading::<Vec<i64>>(Some(value.clone()));
            let _ = optional_reading::<Vec<i64>>(Some(value));
        }
    }
}
