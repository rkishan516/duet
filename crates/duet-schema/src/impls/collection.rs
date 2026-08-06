//! [`SharedState`] for `Vec<T>` and the two string-keyed maps.

use std::collections::{BTreeMap, HashMap};
use std::hash::BuildHasher;

use duet_core::Value;

use crate::decode::DecodeError;
use crate::registry::Registry;
use crate::state::{NotNullable, SharedState};
use crate::ty::Ty;

impl<T: SharedState> SharedState for Vec<T> {
    fn to_value(&self) -> Value {
        Value::List(self.iter().map(SharedState::to_value).collect())
    }

    /// Refuses the **whole list** if any element is refused.
    ///
    /// Dropping the offending element instead would produce a shorter list
    /// whose indices no longer line up with the store's, which is a wrong
    /// answer that looks like a right one — and every index in every path the
    /// application then builds would be off by one. The Dart and TypeScript
    /// list codecs make the same choice.
    ///
    /// # Errors
    ///
    /// [`DecodeError`] if `value` is not a list, or if any element is not a
    /// `T`. An element's error carries its index, so the message locates the
    /// offending element rather than blaming the list.
    fn from_value(value: &Value) -> Result<Self, DecodeError> {
        let Value::List(items) = value else {
            return Err(DecodeError::wrong_type("list", value));
        };
        let mut out = Vec::with_capacity(items.len());
        for (index, item) in items.iter().enumerate() {
            out.push(T::from_value(item).map_err(|e| e.within_index(index))?);
        }
        Ok(out)
    }

    fn schema(registry: &mut Registry) -> Ty {
        T::schema(registry).list()
    }
}

/// A list is never `Value::Null`, whatever its elements are — so
/// `Option<Vec<Option<T>>>` is expressible and `Option<Option<T>>` is not.
impl<T> NotNullable for Vec<T> {}

/// Implements [`SharedState`] for a string-keyed map type.
///
/// # Why `HashMap` is accepted here and `HashSet` is not
///
/// Both iterate in an order that is not a function of their contents. The
/// difference is where that order can land. A map's entries are collected into
/// a [`BTreeMap`] — which is what [`Value::Map`] *is* — so the ordering is
/// discarded before it can reach the wire, and `to_value` stays a function of
/// the value. A set has nowhere to go but a [`Value::List`], where the order is
/// the output: two equal sets would encode to two different byte strings, break
/// change detection, and make a golden file unstable.
macro_rules! string_map_state {
    ($map:ident $(, $hasher:ident)?) => {
        impl<V: SharedState $(, $hasher: BuildHasher + Default)?> SharedState
            for $map<String, V $(, $hasher)?>
        {
            fn to_value(&self) -> Value {
                Value::Map(
                    self.iter()
                        .map(|(key, value)| (key.clone(), value.to_value()))
                        .collect(),
                )
            }

            /// # Errors
            ///
            /// [`DecodeError`] if `value` is not a map, or if any entry's value
            /// is not a `V`. The entry's key is recorded on the error, and it
            /// is guest-chosen text, so the rendered message bounds it.
            fn from_value(value: &Value) -> Result<Self, DecodeError> {
                let Value::Map(entries) = value else {
                    return Err(DecodeError::wrong_type("map", value));
                };
                let mut out = $map::default();
                for (key, entry) in entries {
                    let decoded =
                        V::from_value(entry).map_err(|e| e.within_key(key.clone()))?;
                    out.insert(key.clone(), decoded);
                }
                Ok(out)
            }

            fn schema(registry: &mut Registry) -> Ty {
                V::schema(registry).map()
            }
        }

        impl<V $(, $hasher)?> NotNullable for $map<String, V $(, $hasher)?> {}
    };
}

string_map_state!(BTreeMap);
string_map_state!(HashMap, S);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_list_round_trips() {
        let original = vec![1i64, 2, 3];
        let lowered = original.to_value();
        assert_eq!(
            lowered,
            Value::List(vec![Value::Int(1), Value::Int(2), Value::Int(3)])
        );
        assert_eq!(Vec::<i64>::from_value(&lowered), Ok(original));
    }

    #[test]
    fn an_empty_list_round_trips() {
        assert_eq!(
            Vec::<String>::from_value(&Vec::<String>::new().to_value()),
            Ok(Vec::new())
        );
    }

    #[test]
    fn a_nested_list_round_trips() {
        let original = vec![vec![1i64], vec![], vec![2, 3]];
        assert_eq!(
            Vec::<Vec<i64>>::from_value(&original.to_value()),
            Ok(original)
        );
    }

    #[test]
    fn one_bad_element_refuses_the_whole_list_and_names_its_index() {
        let hostile = Value::List(vec![
            Value::Int(1),
            Value::Str("nope".into()),
            Value::Int(3),
        ]);
        let error = Vec::<i64>::from_value(&hostile).expect_err("element 1 is not an i64");
        assert_eq!(error.to_string(), "expected i64 at [1], found string");
    }

    #[test]
    fn a_non_list_is_refused_as_a_list() {
        let error = Vec::<i64>::from_value(&Value::map([])).expect_err("a map is not a list");
        assert_eq!(error.to_string(), "expected list at <root>, found map");
    }

    #[test]
    fn nested_failures_report_the_whole_location() {
        let hostile = Value::List(vec![
            Value::List(vec![Value::Int(1)]),
            Value::List(vec![Value::Int(1), Value::Bool(true)]),
        ]);
        let error = Vec::<Vec<i64>>::from_value(&hostile).expect_err("[1][1] is not an i64");
        assert_eq!(error.to_string(), "expected i64 at [1][1], found bool");
    }

    #[test]
    fn both_map_kinds_round_trip() {
        let mut btree = BTreeMap::new();
        btree.insert("b".to_string(), 2i64);
        btree.insert("a".to_string(), 1i64);
        let lowered = btree.to_value();
        assert_eq!(
            lowered,
            Value::map([("a", Value::Int(1)), ("b", Value::Int(2))])
        );
        assert_eq!(BTreeMap::<String, i64>::from_value(&lowered), Ok(btree));

        let mut hash = HashMap::new();
        hash.insert("b".to_string(), 2i64);
        hash.insert("a".to_string(), 1i64);
        assert_eq!(hash.to_value(), lowered, "hash order must not reach Value");
        assert_eq!(HashMap::<String, i64>::from_value(&lowered), Ok(hash));
    }

    #[test]
    fn a_hash_maps_iteration_order_never_reaches_the_wire() {
        // The property that makes `HashMap<String, V>` acceptable while
        // `HashSet<T>` is not. Built in two different insertion orders, which
        // is enough to give `RandomState` different bucket orders in practice.
        let forwards: HashMap<String, i64> = ('a'..='z')
            .enumerate()
            .map(|(i, c)| (c.to_string(), i as i64))
            .collect();
        let backwards: HashMap<String, i64> = ('a'..='z')
            .rev()
            .enumerate()
            .map(|(i, c)| (c.to_string(), 25 - i as i64))
            .collect();
        assert_eq!(forwards, backwards);
        assert_eq!(
            forwards.to_value(),
            backwards.to_value(),
            "equal maps must lower to equal values"
        );
    }

    #[test]
    fn one_bad_entry_refuses_the_whole_map_and_names_its_key() {
        let hostile = Value::map([("zoom", Value::Str("nope".into()))]);
        let error = BTreeMap::<String, i64>::from_value(&hostile).expect_err("zoom is not an i64");
        assert_eq!(error.to_string(), "expected i64 at zoom, found string");
    }

    #[test]
    fn a_guest_chosen_map_key_cannot_grow_the_error() {
        let key = "k".repeat(100_000);
        let hostile = Value::Map(BTreeMap::from([(key, Value::Null)]));
        let error = BTreeMap::<String, i64>::from_value(&hostile).expect_err("null is not an i64");
        assert!(
            error.to_string().len() < 256,
            "got {}",
            error.to_string().len()
        );
    }

    #[test]
    fn a_non_map_is_refused_as_a_map() {
        for value in [Value::Null, Value::List(Vec::new()), Value::Int(1)] {
            assert!(
                BTreeMap::<String, i64>::from_value(&value).is_err(),
                "{value:?}"
            );
            assert!(
                HashMap::<String, i64>::from_value(&value).is_err(),
                "{value:?}"
            );
        }
    }

    #[test]
    fn the_container_schemas_wrap_their_element() {
        let mut registry = Registry::new();
        assert_eq!(Vec::<i64>::schema(&mut registry), Ty::Int.list());
        assert_eq!(
            Vec::<Vec<String>>::schema(&mut registry),
            Ty::Str.list().list()
        );
        assert_eq!(
            BTreeMap::<String, bool>::schema(&mut registry),
            Ty::Bool.map()
        );
        assert_eq!(
            HashMap::<String, f64>::schema(&mut registry),
            Ty::Float.map()
        );
    }

    #[test]
    fn collections_are_total_over_hostile_input() {
        let hostile = [
            Value::Null,
            Value::Bool(true),
            Value::Int(1),
            Value::Str("x".into()),
            Value::Bytes(vec![1]),
            Value::List(vec![Value::Null, Value::Int(1)]),
            Value::map([("a", Value::Bytes(vec![]))]),
        ];
        for value in &hostile {
            let _ = Vec::<i64>::from_value(value);
            let _ = Vec::<Vec<String>>::from_value(value);
            let _ = BTreeMap::<String, i64>::from_value(value);
            let _ = HashMap::<String, Value>::from_value(value);
        }
    }
}
