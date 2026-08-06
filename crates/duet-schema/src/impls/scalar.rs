//! [`SharedState`] for the leaf types.

use duet_core::Value;

use crate::decode::DecodeError;
use crate::registry::Registry;
use crate::state::{NotNullable, SharedState};
use crate::ty::Ty;

impl SharedState for bool {
    fn to_value(&self) -> Value {
        Value::Bool(*self)
    }

    fn from_value(value: &Value) -> Result<Self, DecodeError> {
        match value {
            Value::Bool(b) => Ok(*b),
            other => Err(DecodeError::wrong_type("bool", other)),
        }
    }

    fn schema(_registry: &mut Registry) -> Ty {
        Ty::Bool
    }
}

impl NotNullable for bool {}

/// Implements [`SharedState`] for one integer type.
///
/// # The range check is not defensive programming
///
/// The wire has exactly **one** integer type and it is `i64`. A schema that
/// says `u8` is a promise the *Rust* side keeps; nothing stops the Flutter
/// engine or the webview writing `300`, or `-1`, to that path — the two-guest
/// proof has both of them writing one store. `i64::from` is infallible for
/// every type here, so the encode direction never checks; `try_from` on the way
/// back is where a value that does not fit becomes a reportable
/// [`DecodeError::out_of_range`] instead of a wrapped or truncated number.
///
/// `i64` itself goes through the same two conversions — `From<i64> for i64` and
/// `TryFrom<i64> for i64` both exist and are the identity — so there is no
/// special case to get wrong, and the out-of-range arm is simply unreachable
/// for it rather than absent.
macro_rules! integer_state {
    ($($ty:ty),+ $(,)?) => {$(
        impl SharedState for $ty {
            fn to_value(&self) -> Value {
                Value::Int(i64::from(*self))
            }

            fn from_value(value: &Value) -> Result<Self, DecodeError> {
                match value {
                    Value::Int(n) => <$ty>::try_from(*n)
                        .map_err(|_| DecodeError::out_of_range(stringify!($ty), *n)),
                    other => Err(DecodeError::wrong_type(stringify!($ty), other)),
                }
            }

            fn schema(_registry: &mut Registry) -> Ty {
                Ty::Int
            }
        }

        impl NotNullable for $ty {}
    )+};
}

integer_state!(i8, i16, i32, i64, u8, u16, u32);

impl SharedState for f64 {
    fn to_value(&self) -> Value {
        Value::Float(*self)
    }

    /// Accepts only [`Value::Float`].
    ///
    /// An [`Value::Int`] is **not** widened. `Int` and `Float` are distinct
    /// host types with distinct wire tags, and a decoder that quietly took one
    /// for the other would let a guest write an `i64` where the schema says
    /// `f64` and have every other guest agree with it — the exact cross-guest
    /// type drift a mismatch exists to surface. The Dart and TypeScript float
    /// codecs make the identical choice, deliberately.
    fn from_value(value: &Value) -> Result<Self, DecodeError> {
        match value {
            Value::Float(f) => Ok(*f),
            other => Err(DecodeError::wrong_type("f64", other)),
        }
    }

    fn schema(_registry: &mut Registry) -> Ty {
        Ty::Float
    }
}

impl NotNullable for f64 {}

impl SharedState for String {
    fn to_value(&self) -> Value {
        Value::Str(self.clone())
    }

    fn from_value(value: &Value) -> Result<Self, DecodeError> {
        match value {
            Value::Str(s) => Ok(s.clone()),
            other => Err(DecodeError::wrong_type("String", other)),
        }
    }

    fn schema(_registry: &mut Registry) -> Ty {
        Ty::Str
    }
}

impl NotNullable for String {}

/// [`Value`] is its own shared state: the `dynamic` escape hatch.
///
/// Deliberately **not** [`NotNullable`] — a `Value` may *be* `Value::Null`, so
/// `Option<Value>` would collapse `Some(Value::Null)` and `None` into one
/// encoding. See [`NotNullable`].
impl SharedState for Value {
    fn to_value(&self) -> Value {
        self.clone()
    }

    fn from_value(value: &Value) -> Result<Self, DecodeError> {
        Ok(value.clone())
    }

    fn schema(_registry: &mut Registry) -> Ty {
        Ty::Dynamic
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Asserts a type round-trips and that the schema arm is what we claim.
    fn round_trips<T: SharedState + PartialEq + std::fmt::Debug>(
        value: T,
        expected: Value,
        ty: Ty,
    ) {
        let lowered = value.to_value();
        assert_eq!(lowered, expected, "lowering differs");
        assert_eq!(T::from_value(&lowered).as_ref(), Ok(&value));
        assert_eq!(T::schema(&mut Registry::new()), ty);
    }

    #[test]
    fn scalars_round_trip() {
        round_trips(true, Value::Bool(true), Ty::Bool);
        round_trips(false, Value::Bool(false), Ty::Bool);
        round_trips(1.5f64, Value::Float(1.5), Ty::Float);
        round_trips("hi".to_string(), Value::Str("hi".into()), Ty::Str);
        round_trips(Value::Null, Value::Null, Ty::Dynamic);
    }

    #[test]
    fn every_integer_width_round_trips_at_its_extremes() {
        round_trips(i8::MIN, Value::Int(-128), Ty::Int);
        round_trips(i8::MAX, Value::Int(127), Ty::Int);
        round_trips(i16::MIN, Value::Int(-32_768), Ty::Int);
        round_trips(i32::MAX, Value::Int(2_147_483_647), Ty::Int);
        round_trips(i64::MIN, Value::Int(i64::MIN), Ty::Int);
        round_trips(i64::MAX, Value::Int(i64::MAX), Ty::Int);
        round_trips(u8::MAX, Value::Int(255), Ty::Int);
        round_trips(u16::MAX, Value::Int(65_535), Ty::Int);
        round_trips(u32::MAX, Value::Int(4_294_967_295), Ty::Int);
    }

    #[test]
    fn an_integer_that_does_not_fit_is_a_reportable_error_not_a_wrap() {
        // What a guest writing to a `u8` path actually produces.
        let over = <u8 as SharedState>::from_value(&Value::Int(256));
        assert_eq!(
            over.expect_err("256 does not fit u8").to_string(),
            "256 at <root> does not fit u8"
        );
        let under = <u8 as SharedState>::from_value(&Value::Int(-1));
        assert_eq!(
            under.expect_err("-1 does not fit u8").to_string(),
            "-1 at <root> does not fit u8"
        );
    }

    #[test]
    fn every_narrow_integer_type_checks_both_of_its_bounds() {
        macro_rules! check {
            ($($ty:ty),+) => {$(
                let low = i64::from(<$ty>::MIN) - 1;
                let high = i64::from(<$ty>::MAX) + 1;
                assert!(
                    <$ty as SharedState>::from_value(&Value::Int(low)).is_err(),
                    "{low} must not decode as {}", stringify!($ty)
                );
                assert!(
                    <$ty as SharedState>::from_value(&Value::Int(high)).is_err(),
                    "{high} must not decode as {}", stringify!($ty)
                );
                assert!(<$ty as SharedState>::from_value(&Value::Int(i64::from(<$ty>::MIN))).is_ok());
                assert!(<$ty as SharedState>::from_value(&Value::Int(i64::from(<$ty>::MAX))).is_ok());
            )+};
        }
        check!(i8, i16, i32, u8, u16, u32);
    }

    #[test]
    fn i64_accepts_its_whole_domain() {
        // The one integer with no unreachable values: its range check exists
        // only so the macro has no special case, and must never reject.
        for n in [i64::MIN, -1, 0, 1, i64::MAX] {
            assert_eq!(<i64 as SharedState>::from_value(&Value::Int(n)), Ok(n));
        }
    }

    #[test]
    fn an_integer_is_not_a_float_and_a_float_is_not_an_integer() {
        // Pinned deliberately: widening would make a guest's `Int` readable as
        // an `f64` here while Dart and TypeScript refuse it.
        assert!(<f64 as SharedState>::from_value(&Value::Int(1)).is_err());
        assert!(<i64 as SharedState>::from_value(&Value::Float(1.0)).is_err());
    }

    #[test]
    fn every_scalar_decoder_is_total_over_every_value_variant() {
        // A guest can write any of these to any path. None may panic.
        let hostile = [
            Value::Null,
            Value::Bool(true),
            Value::Int(1),
            Value::Float(f64::NAN),
            Value::Str("x".into()),
            Value::Bytes(vec![1]),
            Value::List(vec![Value::Null]),
            Value::map([("k", Value::Null)]),
        ];
        for value in &hostile {
            let _ = <bool as SharedState>::from_value(value);
            let _ = <i8 as SharedState>::from_value(value);
            let _ = <i64 as SharedState>::from_value(value);
            let _ = <u32 as SharedState>::from_value(value);
            let _ = <f64 as SharedState>::from_value(value);
            let _ = <String as SharedState>::from_value(value);
            // `Value` equality is not reflexive for NaN, so this asserts the
            // decode *succeeded* rather than that it round-tripped; the round
            // trip itself is pinned by `scalars_round_trip` and by
            // `a_nan_float_survives_the_round_trip_as_a_nan`.
            assert!(
                <Value as SharedState>::from_value(value).is_ok(),
                "dynamic accepts everything, including {value:?}"
            );
        }
    }

    #[test]
    fn wrong_type_messages_name_the_rust_type() {
        assert_eq!(
            <bool as SharedState>::from_value(&Value::Int(1))
                .expect_err("not a bool")
                .to_string(),
            "expected bool at <root>, found int"
        );
        assert_eq!(
            <String as SharedState>::from_value(&Value::Bytes(vec![]))
                .expect_err("not a String")
                .to_string(),
            "expected String at <root>, found bytes"
        );
        assert_eq!(
            <u16 as SharedState>::from_value(&Value::Null)
                .expect_err("not a u16")
                .to_string(),
            "expected u16 at <root>, found null"
        );
    }

    #[test]
    fn a_nan_float_survives_the_round_trip_as_a_nan() {
        // `Value` equality is not reflexive for NaN, so this cannot use
        // `round_trips`. It is still worth pinning: `to_value` must not
        // normalize, and `from_value` must not reject.
        let decoded = <f64 as SharedState>::from_value(&f64::NAN.to_value());
        assert!(decoded.expect("NaN decodes").is_nan());
    }
}
