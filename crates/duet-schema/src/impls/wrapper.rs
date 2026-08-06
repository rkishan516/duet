//! [`SharedState`] for `Option<T>`, `Box<T>` and `Arc<T>`.

use std::sync::Arc;

use duet_core::Value;

use crate::decode::DecodeError;
use crate::registry::Registry;
use crate::state::{NotNullable, SharedState};
use crate::ty::Ty;

/// `None` is [`Value::Null`]; `Some(x)` is exactly what `x` lowers to.
///
/// # The `NotNullable` bound is the whole design
///
/// Without it this impl would accept `Option<Option<T>>`, and
/// `Some(None)` and `None` would both lower to `Value::Null` — one encoding for
/// two distinct Rust values, with no decoder able to tell them apart. The bound
/// makes that shape have no impl at all, which is a compile error naming the
/// fix rather than a silent collapse at runtime. The same bound rules out
/// `Option<duet_core::Value>`, since a `Value` may itself *be* `Value::Null`.
///
/// # `None` is not "absent"
///
/// `Value::Null` occupies the map key; it does not remove it. The store has no
/// `remove`, so "key absent" means something different — the struct that would
/// contain this field is itself `None`, or was never written. The typed layer
/// keeps the two apart as [`Reading::None`](crate::Reading::None) and
/// [`Reading::Absent`](crate::Reading::Absent).
impl<T: SharedState + NotNullable> SharedState for Option<T> {
    fn to_value(&self) -> Value {
        match self {
            Some(inner) => inner.to_value(),
            None => Value::Null,
        }
    }

    /// # Errors
    ///
    /// [`DecodeError`] if `value` is neither [`Value::Null`] nor a `T`.
    fn from_value(value: &Value) -> Result<Self, DecodeError> {
        match value {
            Value::Null => Ok(None),
            other => T::from_value(other).map(Some),
        }
    }

    fn schema(registry: &mut Registry) -> Ty {
        T::schema(registry).optional()
    }
}

// `Option<T>` is deliberately NOT `NotNullable`; that absence is what rejects
// `Option<Option<T>>`.

/// Implements [`SharedState`] transparently for a smart pointer.
///
/// The pointer is not part of the shared state: `Box<i64>` and `i64` occupy the
/// same path and lower to the same [`Value`]. That is the only honest choice —
/// the store holds a tree of values, and a schema describing "an `i64`, but
/// heap-allocated" would be describing a Rust implementation detail to Dart.
///
/// [`Rc`](std::rc::Rc) is *not* here, and its absence is deliberate rather than
/// an oversight: `Arc` is accepted because sharing it across the store boundary
/// is at least sound, while `Rc` is not `Send` and could not reach the store's
/// core thread. Both, however, lose their sharing — two `Arc`s pointing at one
/// node become two independent copies once they are values in the tree — which
/// is why interior mutability (`RefCell`, `Mutex`, `RwLock`) is refused
/// outright rather than silently copied.
macro_rules! transparent_state {
    ($ptr:ident) => {
        impl<T: SharedState> SharedState for $ptr<T> {
            fn to_value(&self) -> Value {
                (**self).to_value()
            }

            /// # Errors
            ///
            /// Whatever the inner type's decode returns; this adds nothing.
            fn from_value(value: &Value) -> Result<Self, DecodeError> {
                T::from_value(value).map($ptr::new)
            }

            fn schema(registry: &mut Registry) -> Ty {
                T::schema(registry)
            }
        }

        /// Transparent here too: `Option<Box<Option<T>>>` must be refused for
        /// exactly the reason `Option<Option<T>>` is, and would not be if the
        /// pointer erased the inner type's nullability.
        impl<T: NotNullable> NotNullable for $ptr<T> {}
    };
}

transparent_state!(Box);
transparent_state!(Arc);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn none_is_null_and_some_is_transparent() {
        assert_eq!(None::<i64>.to_value(), Value::Null);
        assert_eq!(Some(7i64).to_value(), Value::Int(7));
        assert_eq!(Option::<i64>::from_value(&Value::Null), Ok(None));
        assert_eq!(Option::<i64>::from_value(&Value::Int(7)), Ok(Some(7)));
    }

    #[test]
    fn a_wrong_typed_some_is_still_an_error() {
        // `Option` must not swallow a mismatch into `None`: a guest writing a
        // string where the schema says `Option<i64>` is a disagreement to
        // report, not a value to forget.
        let error = Option::<i64>::from_value(&Value::Str("nope".into()))
            .expect_err("a string is not an Option<i64>");
        assert_eq!(error.to_string(), "expected i64 at <root>, found string");
    }

    #[test]
    fn the_optional_schema_wraps_its_inner_type() {
        let mut registry = Registry::new();
        assert_eq!(Option::<i64>::schema(&mut registry), Ty::Int.optional());
        assert_eq!(
            Option::<Vec<String>>::schema(&mut registry),
            Ty::Str.list().optional()
        );
    }

    #[test]
    fn smart_pointers_are_invisible_to_the_store() {
        // Bound to locals rather than written inline: `Box::new(x).method()` is
        // an allocation the compiler can see is pointless, and `unused_allocation`
        // rejects it — which would be a lint about the *test*, not about the impl
        // it is checking.
        let boxed: Box<i64> = Box::new(7);
        let shared: Arc<i64> = Arc::new(7);
        assert_eq!(boxed.to_value(), Value::Int(7));
        assert_eq!(shared.to_value(), Value::Int(7));
        assert_eq!(Box::<i64>::from_value(&Value::Int(7)), Ok(Box::new(7)));
        assert_eq!(Arc::<i64>::from_value(&Value::Int(7)), Ok(Arc::new(7)));

        let mut registry = Registry::new();
        assert_eq!(Box::<i64>::schema(&mut registry), Ty::Int);
        assert_eq!(Arc::<Vec<i64>>::schema(&mut registry), Ty::Int.list());
    }

    #[test]
    fn a_smart_pointer_propagates_a_decode_failure() {
        assert!(Box::<i64>::from_value(&Value::Null).is_err());
        assert!(Arc::<String>::from_value(&Value::Int(1)).is_err());
    }

    #[test]
    fn an_option_of_a_pointer_still_works() {
        let boxed: Option<Box<i64>> = Some(Box::new(7));
        assert_eq!(boxed.to_value(), Value::Int(7));
        assert_eq!(None::<Box<i64>>.to_value(), Value::Null);
        assert_eq!(
            Option::<Arc<String>>::from_value(&Value::Str("hi".into())),
            Ok(Some(Arc::new("hi".to_string())))
        );
    }

    #[test]
    fn options_are_total_over_hostile_input() {
        for value in [
            Value::Null,
            Value::Bool(true),
            Value::Int(1),
            Value::Float(1.0),
            Value::Str("x".into()),
            Value::Bytes(vec![1]),
            Value::List(Vec::new()),
            Value::map([]),
        ] {
            let _ = Option::<i64>::from_value(&value);
            let _ = Option::<Vec<Option<String>>>::from_value(&value);
            let _ = Box::<Option<i64>>::from_value(&value);
        }
    }

    #[test]
    fn a_list_of_options_keeps_its_nulls() {
        let original = vec![Some(1i64), None, Some(3)];
        let lowered = original.to_value();
        assert_eq!(
            lowered,
            Value::List(vec![Value::Int(1), Value::Null, Value::Int(3)])
        );
        assert_eq!(Vec::<Option<i64>>::from_value(&lowered), Ok(original));
    }
}
