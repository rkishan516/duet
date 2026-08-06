//! Binary data that lowers to [`Value::Bytes`].

use duet_core::Value;

use crate::decode::DecodeError;
use crate::registry::Registry;
use crate::state::{NotNullable, SharedState};
use crate::ty::Ty;

/// Binary data, lowered to [`Value::Bytes`] rather than to a list of integers.
///
/// # Why this newtype exists, and why it is a deviation worth naming
///
/// The obvious spelling for binary data is `Vec<u8>`, and the original design
/// said `Vec<u8>` would lower to [`Value::Bytes`]. It cannot, and no amount of
/// care in this crate changes that. `u8` is accepted as an integer, so
/// `impl<T: SharedState> SharedState for Vec<T>` already covers `Vec<u8>`, and
/// a second `impl SharedState for Vec<u8>` overlaps it. Rust's coherence rules
/// reject the pair outright — this is not a lint that could be silenced, and
/// stable Rust has no specialization to break the tie.
///
/// So the two spellings are separated at the type level, and the consequence is
/// stated here rather than buried:
///
/// | Rust | `Value` | Wire |
/// |---|---|---|
/// | `Vec<u8>` | `List([Int, Int, …])` | one tagged object per byte |
/// | [`Bytes`] | `Bytes([…])` | one base64 string |
///
/// Both are correct; they are not interchangeable. `Vec<u8>` is what you want
/// for "a handful of small numbers that happen to fit a byte". [`Bytes`] is
/// what you want for an image, a hash, or anything where one tagged JSON object
/// per byte would be absurd — which is nearly every actual use of `Vec<u8>`.
///
/// The failure mode of choosing wrong is loud rather than silent: a guest
/// reading a `bytes` field finds a list, and the typed guest runtime reports a
/// mismatch. Nothing decodes to the wrong thing.
///
/// ```
/// use duet_core::Value;
/// use duet_schema::{Bytes, SharedState};
///
/// assert_eq!(Bytes::from(vec![1u8, 2]).to_value(), Value::Bytes(vec![1, 2]));
/// assert_eq!(
///     vec![1u8, 2].to_value(),
///     Value::List(vec![Value::Int(1), Value::Int(2)])
/// );
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Bytes(pub Vec<u8>);

impl Bytes {
    /// Borrows the bytes.
    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }

    /// Consumes this and yields the bytes.
    #[must_use]
    pub fn into_vec(self) -> Vec<u8> {
        self.0
    }
}

impl From<Vec<u8>> for Bytes {
    fn from(bytes: Vec<u8>) -> Bytes {
        Bytes(bytes)
    }
}

impl From<Bytes> for Vec<u8> {
    fn from(bytes: Bytes) -> Vec<u8> {
        bytes.0
    }
}

impl AsRef<[u8]> for Bytes {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl SharedState for Bytes {
    fn to_value(&self) -> Value {
        Value::Bytes(self.0.clone())
    }

    fn from_value(value: &Value) -> Result<Self, DecodeError> {
        match value {
            Value::Bytes(bytes) => Ok(Bytes(bytes.clone())),
            other => Err(DecodeError::wrong_type("Bytes", other)),
        }
    }

    fn schema(_registry: &mut Registry) -> Ty {
        Ty::Bytes
    }
}

impl NotNullable for Bytes {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_round_trip_through_the_bytes_variant() {
        let original = Bytes(vec![0xDE, 0xAD, 0xBE, 0xEF]);
        let lowered = original.to_value();
        assert_eq!(lowered, Value::Bytes(vec![0xDE, 0xAD, 0xBE, 0xEF]));
        assert_eq!(Bytes::from_value(&lowered), Ok(original));
    }

    #[test]
    fn empty_bytes_round_trip() {
        assert_eq!(
            Bytes::from_value(&Bytes::default().to_value()),
            Ok(Bytes(Vec::new()))
        );
    }

    #[test]
    fn a_list_of_integers_is_not_bytes() {
        // The measured consequence of the deviation this type documents: a
        // `Vec<u8>` written by another guest does not decode as `Bytes`.
        let as_list = vec![1u8, 2].to_value();
        let error = Bytes::from_value(&as_list).expect_err("a list is not bytes");
        assert_eq!(error.to_string(), "expected Bytes at <root>, found list");
    }

    #[test]
    fn every_other_variant_is_refused() {
        for value in [
            Value::Null,
            Value::Bool(true),
            Value::Int(1),
            Value::Float(1.0),
            Value::Str("AA==".into()),
            Value::List(Vec::new()),
            Value::map([]),
        ] {
            assert!(Bytes::from_value(&value).is_err(), "{value:?} is not Bytes");
        }
    }

    #[test]
    fn the_conversions_are_mutually_inverse() {
        let raw = vec![7u8, 8, 9];
        let wrapped = Bytes::from(raw.clone());
        assert_eq!(wrapped.as_slice(), raw.as_slice());
        assert_eq!(wrapped.as_ref(), raw.as_slice());
        assert_eq!(Vec::<u8>::from(wrapped.clone()), raw);
        assert_eq!(wrapped.into_vec(), raw);
    }

    #[test]
    fn the_schema_arm_is_bytes_not_a_list() {
        let mut registry = Registry::new();
        assert_eq!(Bytes::schema(&mut registry), Ty::Bytes);
        assert_eq!(
            <Vec<u8> as SharedState>::schema(&mut registry),
            Ty::Int.list()
        );
    }
}
