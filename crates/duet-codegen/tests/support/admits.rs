//! Does a schema type admit a value? An independent second opinion.
//!
//! `schema_corpus::admitted` and `schema_corpus::rejected` *claim* which values
//! each type takes. This decides it, and the two are deliberately written apart:
//! a corpus whose accept and reject values were checked by the same code that
//! built them would state whatever that code believed, and a guest failing
//! against it could not tell a wrong corpus from a wrong guest.
//!
//! Structured as a predicate over `(Value, Ty)` — a type-directed validator —
//! rather than as a table of the values the builders happen to produce. So it
//! answers for values the builders never make, which is what makes it a check
//! rather than a restatement.
//!
//! # This is not a decoder
//!
//! It answers *whether* a value conforms, never what it decodes to, and it is
//! not what any guest runs. The guests decode through their generated codecs;
//! this exists so the file those guests are handed cannot be wrong in the same
//! direction as the thing that wrote it.

use duet_core::Value;
use duet_schema::{Ty, TypeDef};

/// True if `value` is a legal inhabitant of `ty`.
///
/// `optional` widens it by exactly [`Value::Null`], mirroring how the two guest
/// runtimes split `DuetField` from `DuetOptionalField`: the optionality lives
/// beside the type, never inside the codec.
pub fn admits(value: &Value, ty: &Ty, optional: bool, types: &[TypeDef]) -> bool {
    if optional && matches!(value, Value::Null) {
        return true;
    }
    conforms(value, ty, types)
}

/// True if `value` inhabits `ty` itself, with no optionality allowed.
fn conforms(value: &Value, ty: &Ty, types: &[TypeDef]) -> bool {
    match (ty, value) {
        (Ty::Bool, Value::Bool(_)) => true,
        (Ty::Int, Value::Int(_)) => true,
        (Ty::Float, Value::Float(_)) => true,
        (Ty::Str, Value::Str(_)) => true,
        (Ty::Bytes, Value::Bytes(_)) => true,
        // The identity type: every value the wire can carry, `Null` included.
        (Ty::Dynamic, _) => true,
        (Ty::Optional(inner), _) => matches!(value, Value::Null) || conforms(value, inner, types),
        (Ty::List(inner), Value::List(items)) => {
            items.iter().all(|item| conforms(item, inner, types))
        }
        (Ty::Map(inner), Value::Map(entries)) => {
            entries.values().all(|item| conforms(item, inner, types))
        }
        (Ty::Named(name), Value::Map(entries)) => struct_conforms(name, entries, types),
        _ => false,
    }
}

/// True if `entries` holds exactly the fields `name` declares, each conforming.
///
/// **Exactly**, in both directions. A missing key is a value the struct's
/// generated codec refuses, and so is a value it can decode but not re-encode:
/// an extra key survives a decode and vanishes on the way back out, which is a
/// silent loss of another guest's write. Both guests' generated decoders read
/// only the keys they know, so the extra-key half is the stricter of the two
/// and is stated here rather than in the corpus, where it would be an
/// unenforceable claim.
fn struct_conforms(
    name: &str,
    entries: &std::collections::BTreeMap<String, Value>,
    types: &[TypeDef],
) -> bool {
    let Some(def) = types.iter().find(|t| t.name == name) else {
        return false;
    };
    if entries.len() != def.fields.len() {
        return false;
    }
    def.fields.iter().all(|field| {
        entries
            .get(&field.key)
            .is_some_and(|held| conforms(held, &field.ty, types))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn editor() -> Vec<TypeDef> {
        vec![TypeDef {
            name: "Editor".into(),
            fields: vec![
                duet_schema::FieldDef::new("zoom", Ty::Float),
                duet_schema::FieldDef::new("theme", Ty::Str),
            ],
        }]
    }

    fn full_editor() -> Value {
        Value::map([
            ("zoom", Value::Float(1.0)),
            ("theme", Value::Str("dark".into())),
        ])
    }

    #[test]
    fn an_int_is_not_a_float_and_a_float_is_not_an_int() {
        // The distinction the whole increment turns on. `Value::Int` and
        // `Value::Float` are separate variants on the host, and a validator
        // that widened one into the other would bless the exact codec bug this
        // corpus exists to catch.
        assert!(admits(&Value::Int(1), &Ty::Int, false, &[]));
        assert!(!admits(&Value::Int(1), &Ty::Float, false, &[]));
        assert!(admits(&Value::Float(1.0), &Ty::Float, false, &[]));
        assert!(!admits(&Value::Float(1.0), &Ty::Int, false, &[]));
    }

    #[test]
    fn bytes_are_not_a_list_of_integers_and_not_base64_text() {
        assert!(admits(&Value::Bytes(vec![1]), &Ty::Bytes, false, &[]));
        assert!(!admits(
            &Value::List(vec![Value::Int(1)]),
            &Ty::Bytes,
            false,
            &[]
        ));
        assert!(!admits(&Value::Str("AQID".into()), &Ty::Bytes, false, &[]));
    }

    #[test]
    fn dynamic_admits_everything_including_null() {
        for value in [
            Value::Null,
            Value::Bool(false),
            Value::Int(0),
            Value::Str(String::new()),
            Value::List(Vec::new()),
        ] {
            assert!(admits(&value, &Ty::Dynamic, false, &[]), "{value:?}");
        }
    }

    #[test]
    fn null_is_admitted_by_an_optional_field_and_by_nothing_else() {
        assert!(admits(&Value::Null, &Ty::Str, true, &[]));
        assert!(!admits(&Value::Null, &Ty::Str, false, &[]));
        assert!(admits(&Value::Str("a".into()), &Ty::Str, true, &[]));
    }

    #[test]
    fn a_container_checks_its_elements_and_not_only_its_kind() {
        let floats = Ty::Float.list();
        assert!(admits(
            &Value::List(vec![Value::Float(1.0)]),
            &floats,
            false,
            &[]
        ));
        assert!(!admits(
            &Value::List(vec![Value::Int(1)]),
            &floats,
            false,
            &[]
        ));
        assert!(!admits(&Value::Map(BTreeMap::new()), &floats, false, &[]));

        let ints = Ty::Int.map();
        assert!(admits(
            &Value::map([("k", Value::Int(1))]),
            &ints,
            false,
            &[]
        ));
        assert!(!admits(
            &Value::map([("k", Value::Str("1".into()))]),
            &ints,
            false,
            &[]
        ));
        assert!(!admits(&Value::List(Vec::new()), &ints, false, &[]));
    }

    #[test]
    fn a_struct_needs_every_field_and_admits_no_extra_one() {
        let types = editor();
        let named = Ty::Named("Editor".into());
        assert!(admits(&full_editor(), &named, false, &types));

        assert!(
            !admits(
                &Value::map([("zoom", Value::Float(1.0))]),
                &named,
                false,
                &types
            ),
            "a missing field must be refused"
        );
        assert!(
            !admits(
                &Value::map([
                    ("zoom", Value::Float(1.0)),
                    ("theme", Value::Str("dark".into())),
                    ("extra", Value::Int(1)),
                ]),
                &named,
                false,
                &types
            ),
            "an extra field survives a decode and vanishes on re-encode"
        );
        assert!(
            !admits(
                &Value::map([
                    ("zoom", Value::Int(1)),
                    ("theme", Value::Str("dark".into())),
                ]),
                &named,
                false,
                &types
            ),
            "a field of the wrong type must be refused"
        );
        assert!(!admits(&Value::Int(1), &named, false, &types));
    }

    #[test]
    fn a_name_that_does_not_resolve_admits_nothing() {
        assert!(!admits(
            &Value::Map(BTreeMap::new()),
            &Ty::Named("Nowhere".into()),
            false,
            &[]
        ));
    }

    #[test]
    fn an_optional_nested_in_a_type_still_admits_null() {
        // `Ty::Optional` can appear inside a `Ty` a caller assembled by hand
        // even though `Plan` lifts it out of every field it emits.
        assert!(conforms(&Value::Null, &Ty::Str.optional(), &[]));
        assert!(conforms(&Value::Str("a".into()), &Ty::Str.optional(), &[]));
        assert!(!conforms(&Value::Int(1), &Ty::Str.optional(), &[]));
    }
}
