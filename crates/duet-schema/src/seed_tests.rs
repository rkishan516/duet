//! Tests for [`super::seed`].

use super::*;
use crate::ty::FieldDef;
use duet_core::{MAX_VALUE_DEPTH, Path};

/// A one-field schema whose root field has type `ty`.
fn schema_of(ty: Ty) -> Schema {
    Schema::build(
        Ty::Named("Root".into()),
        vec![TypeDef {
            name: "Root".into(),
            fields: vec![FieldDef::new("f", ty)],
        }],
    )
    .expect("a one-field schema is valid")
}

/// The seed at `Root.f`.
fn seeded_field(ty: Ty) -> Value {
    let schema = schema_of(ty);
    let root = seed(&schema);
    let path = Path::parse("f").expect("a legal path");
    root.get(&path).cloned().expect("the field must be seeded")
}

#[test]
fn every_scalar_arm_seeds_a_value_of_its_own_kind() {
    // Pinned one arm at a time, and by value rather than by kind: an `int`
    // seeded as `Float(0.0)` would still be "a number", and a guest whose
    // codec is a float codec would read it happily. Exactly the mistake this
    // whole increment exists to catch.
    assert_eq!(seeded_field(Ty::Bool), Value::Bool(false));
    assert_eq!(seeded_field(Ty::Int), Value::Int(0));
    assert_eq!(seeded_field(Ty::Float), Value::Float(0.0));
    assert_eq!(seeded_field(Ty::Str), Value::Str(String::new()));
    assert_eq!(seeded_field(Ty::Bytes), Value::Bytes(Vec::new()));
    assert_eq!(seeded_field(Ty::Dynamic), Value::Null);
}

#[test]
fn a_container_seeds_empty_rather_than_holding_one_of_its_element_type() {
    // A `list<T>` holding one seeded `T` would make every generated list
    // accessor read a one-element list before anything was written, and a
    // guest asserting "the seed is empty" would be asserting a lie.
    assert_eq!(seeded_field(Ty::Int.list()), Value::List(Vec::new()));
    assert_eq!(
        seeded_field(Ty::Int.map()),
        Value::Map(std::collections::BTreeMap::new())
    );
}

#[test]
fn an_optional_seeds_null_and_not_its_inner_types_seed() {
    // THE case the `Option` behaviours depend on. If `Option<Editor>` seeded
    // as a map, `maybe_editor.zoom` would address a real node and the three
    // measured behaviours — get null, set fails, subscribe succeeds — would be
    // unreachable from a seeded store.
    assert_eq!(seeded_field(Ty::Str.optional()), Value::Null);

    let schema = Schema::build(
        Ty::Named("Root".into()),
        vec![
            TypeDef {
                name: "Root".into(),
                fields: vec![FieldDef::new("f", Ty::Named("Leaf".into()).optional())],
            },
            TypeDef {
                name: "Leaf".into(),
                fields: vec![FieldDef::new("zoom", Ty::Float)],
            },
        ],
    )
    .expect("a valid schema");
    assert_eq!(seed(&schema), Value::map([("f", Value::Null)]));
}

#[test]
fn a_named_field_seeds_every_declared_key() {
    let schema = Schema::build(
        Ty::Named("Outer".into()),
        vec![
            TypeDef {
                name: "Inner".into(),
                fields: vec![
                    FieldDef::new("zoom", Ty::Float),
                    FieldDef::new("theme", Ty::Str),
                ],
            },
            TypeDef {
                name: "Outer".into(),
                fields: vec![
                    FieldDef::new("inner", Ty::Named("Inner".into())),
                    FieldDef::new("depth", Ty::Int),
                ],
            },
        ],
    )
    .expect("a valid schema");

    assert_eq!(
        seed(&schema),
        Value::map([
            (
                "inner",
                Value::map([
                    ("zoom", Value::Float(0.0)),
                    ("theme", Value::Str(String::new()))
                ]),
            ),
            ("depth", Value::Int(0)),
        ]),
    );
}

#[test]
fn a_seeded_store_accepts_a_write_at_every_path_the_schema_mints() {
    // The property the seed exists for. `Value::set` never creates
    // intermediate nodes, so a seed that missed a struct would turn every
    // write below it into a refusal — and a conformance run against such a
    // host would be testing nothing but the refusal path.
    let schema = Schema::build(
        Ty::Named("App".into()),
        vec![
            TypeDef {
                name: "App".into(),
                fields: vec![
                    FieldDef::new("counter", Ty::Int),
                    FieldDef::new("editor", Ty::Named("Editor".into())),
                ],
            },
            TypeDef {
                name: "Editor".into(),
                fields: vec![FieldDef::new("zoom", Ty::Float)],
            },
        ],
    )
    .expect("a valid schema");

    let mut root = seed(&schema);
    for (path, value) in [
        ("counter", Value::Int(7)),
        ("editor.zoom", Value::Float(3.25)),
    ] {
        let parsed = Path::parse(path).expect("a legal path");
        root.set(&parsed, value.clone())
            .unwrap_or_else(|e| panic!("a write at {path} must land on a seeded store: {e}"));
        assert_eq!(root.get(&parsed), Some(&value));
    }
}

#[test]
fn a_child_of_an_optional_struct_is_absent_and_refuses_a_write() {
    // The three measured behaviours, at the smallest scale that can express
    // them, on a store seeded by this function. `packages/duet`'s fake host
    // transcribes these; the live-host conformance run checks the
    // transcription against the real thing.
    let schema = Schema::build(
        Ty::Named("Wide".into()),
        vec![
            TypeDef {
                name: "Editor".into(),
                fields: vec![FieldDef::new("zoom", Ty::Float)],
            },
            TypeDef {
                name: "Wide".into(),
                fields: vec![FieldDef::new(
                    "maybe_editor",
                    Ty::Named("Editor".into()).optional(),
                )],
            },
        ],
    )
    .expect("a valid schema");

    let mut root = seed(&schema);
    let parent = Path::parse("maybe_editor").expect("a legal path");
    let child = Path::parse("maybe_editor.zoom").expect("a legal path");

    assert_eq!(root.get(&parent), Some(&Value::Null), "None, not absent");
    assert_eq!(root.get(&child), None, "absent, not None");
    let refused = root
        .set(&child, Value::Float(1.0))
        .expect_err("a write below a None struct must be refused");
    assert!(
        refused.to_string().contains("wrong kind of node"),
        "the refusal must say why, got {refused}"
    );
}

#[test]
fn a_container_seeds_without_looking_inside_itself() {
    // The mechanism behind the recursion bound, asserted directly rather than
    // inferred from a stack that did not happen to overflow: a `list` of a
    // name that does not resolve still seeds, which is only possible if the
    // walk never descended into it. So the recursion depth is *struct*
    // nesting, which `Schema` has already bounded by `MAX_VALUE_DEPTH`, and
    // not `Ty` nesting, which a container can make arbitrarily deep.
    assert_eq!(
        seeded(&Ty::Named("Nowhere".into()).list(), &[]),
        Value::List(Vec::new())
    );
    assert_eq!(
        seeded(&Ty::Named("Nowhere".into()).map(), &[]),
        Value::Map(std::collections::BTreeMap::new())
    );
    assert_eq!(
        seeded(&Ty::Named("Nowhere".into()).optional(), &[]),
        Value::Null
    );

    // And a `Ty` nested far past the schema's own bound seeds all the same.
    // Kept to a depth `Ty`'s own recursive `Drop` survives: a boxed chain tens
    // of thousands deep overflows the stack when it is *freed*, which is a
    // property of `Ty` rather than of this function, and is unreachable
    // through `read_schema` (bounded by `MAX_TY_DEPTH`) or `Schema::build`
    // (bounded by `MAX_VALUE_DEPTH`).
    let mut ty = Ty::Int;
    for _ in 0..(MAX_VALUE_DEPTH * 4) {
        ty = ty.list();
    }
    assert_eq!(seeded(&ty, &[]), Value::List(Vec::new()));
}

#[test]
fn a_dangling_named_reference_seeds_null_rather_than_panicking() {
    // Unreachable through `seed`, because `Schema` rejects a dangling
    // `Ty::Named`. Reachable through `seeded_struct` directly, and answering
    // is what keeps the walk total for a hand-assembled `TypeDef` list.
    assert_eq!(seeded_struct("Nowhere", &[]), Value::Null);
}
