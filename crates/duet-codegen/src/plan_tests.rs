//! Unit tests for the plan: what it expands, what it names, and what it
//! refuses. The rejection *messages* are pinned in `tests/negative.rs`.

use super::*;

use duet_schema::{FieldDef, Schema};

/// A schema with `root` as its root and `types` as its definitions.
fn schema(root: &str, types: Vec<TypeDef>) -> Schema {
    Schema::build(Ty::Named(root.to_string()), types).expect("a legal schema")
}

/// One struct.
fn def(name: &str, fields: Vec<FieldDef>) -> TypeDef {
    TypeDef {
        name: name.to_string(),
        fields,
    }
}

#[test]
fn the_root_class_is_named_after_the_root_type() {
    let plan = Plan::build(&schema(
        "App",
        vec![def("App", vec![FieldDef::new("counter", Ty::Int)])],
    ))
    .expect("emittable");
    assert_eq!(plan.root, "App");
    assert_eq!(plan.classes[0].name, "AppClient");
    assert_eq!(plan.classes[0].path, "");
    assert!(!plan.classes[0].optional);
}

#[test]
fn a_struct_field_expands_into_a_class_whose_paths_carry_the_prefix() {
    let plan = Plan::build(&schema(
        "App",
        vec![
            def(
                "App",
                vec![FieldDef::new("editor", Ty::Named("Editor".to_string()))],
            ),
            def("Editor", vec![FieldDef::new("zoom", Ty::Float)]),
        ],
    ))
    .expect("emittable");
    let nested = &plan.classes[1];
    assert_eq!(nested.name, "AppEditorClient");
    assert_eq!(nested.path, "editor");
    assert_eq!(nested.accessors[0].path, "editor.zoom");
}

#[test]
fn one_type_reached_twice_gets_one_class_per_path_not_per_type() {
    // `client.left.zoom` and `client.right.zoom` are different paths; a single
    // shared class could only hold one of them as a literal.
    let plan = Plan::build(&schema(
        "App",
        vec![
            def(
                "App",
                vec![
                    FieldDef::new("left", Ty::Named("Editor".to_string())),
                    FieldDef::new("right", Ty::Named("Editor".to_string())),
                ],
            ),
            def("Editor", vec![FieldDef::new("zoom", Ty::Float)]),
        ],
    ))
    .expect("emittable");
    let names: Vec<&str> = plan.classes.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["AppClient", "AppLeftClient", "AppRightClient"]);
    assert_eq!(plan.classes[1].accessors[0].path, "left.zoom");
    assert_eq!(plan.classes[2].accessors[0].path, "right.zoom");
}

#[test]
fn an_optional_struct_field_marks_its_class_optional() {
    let plan = Plan::build(&schema(
        "App",
        vec![
            def(
                "App",
                vec![FieldDef::new(
                    "editor",
                    Ty::Named("Editor".to_string()).optional(),
                )],
            ),
            def("Editor", vec![FieldDef::new("zoom", Ty::Float)]),
        ],
    ))
    .expect("emittable");
    assert!(plan.classes[1].optional, "the class lost its optionality");
    // The children of an optional struct are *not* themselves optional: the
    // schema promises an `f64` at `editor.zoom` whenever `editor` is present,
    // and reports absent when it is not.
    assert!(!plan.classes[1].accessors[0].ty.optional);
}

#[test]
fn a_struct_inside_a_container_is_not_expanded() {
    // A list index and a map key are runtime values. Expanding into them would
    // need a path built at runtime, which is the one thing generated code may
    // not do.
    let plan = Plan::build(&schema(
        "App",
        vec![
            def(
                "App",
                vec![
                    FieldDef::new("many", Ty::Named("Editor".to_string()).list()),
                    FieldDef::new("keyed", Ty::Named("Editor".to_string()).map()),
                ],
            ),
            def("Editor", vec![FieldDef::new("zoom", Ty::Float)]),
        ],
    ))
    .expect("emittable");
    assert_eq!(plan.classes.len(), 1, "a container was expanded into");
    assert!(plan.classes[0].accessors.iter().all(|a| a.nested.is_none()));
}

#[test]
fn the_optionality_of_a_field_is_lifted_out_of_its_type() {
    let plan = Plan::build(&schema(
        "App",
        vec![def(
            "App",
            vec![
                FieldDef::new("plain", Ty::Str),
                FieldDef::new("maybe", Ty::Str.optional()),
                FieldDef::new("maybe_list", Ty::Float.list().optional()),
            ],
        )],
    ))
    .expect("emittable");
    let fields = &plan.types[0].fields;
    assert_eq!(
        fields[0].ty,
        PlannedTy {
            optional: false,
            inner: Ty::Str
        }
    );
    assert_eq!(
        fields[1].ty,
        PlannedTy {
            optional: true,
            inner: Ty::Str
        }
    );
    assert_eq!(
        fields[2].ty,
        PlannedTy {
            optional: true,
            inner: Ty::Float.list()
        }
    );
}

#[test]
fn a_diamond_deep_enough_to_expand_without_bound_is_refused() {
    // Twenty levels, each doubling: the class count is 2^20 if nothing stops
    // it. The cap has to fire *while* expanding rather than after, or the
    // rejection arrives having already allocated what it was preventing.
    let mut types = vec![def("L20", vec![FieldDef::new("leaf", Ty::Int)])];
    for level in (0..20).rev() {
        types.push(def(
            &format!("L{level}"),
            vec![
                FieldDef::new("a", Ty::Named(format!("L{}", level + 1))),
                FieldDef::new("b", Ty::Named(format!("L{}", level + 1))),
            ],
        ));
    }
    let error = Plan::build(&schema("L0", types)).expect_err("an unbounded expansion");
    assert_eq!(error, EmitError::TooManyClasses { max: MAX_CLASSES });
}

#[test]
fn a_deep_chain_that_stays_under_the_cap_is_expanded_whole() {
    // The other side of the boundary: depth alone is fine, it is *branching*
    // that explodes. Fifty links is well past anything real and still linear.
    let mut types = vec![def("L50", vec![FieldDef::new("leaf", Ty::Int)])];
    for level in (0..50).rev() {
        types.push(def(
            &format!("L{level}"),
            vec![FieldDef::new("next", Ty::Named(format!("L{}", level + 1)))],
        ));
    }
    let plan = Plan::build(&schema("L0", types)).expect("a chain is not a diamond");
    assert_eq!(plan.classes.len(), 51);
    let deepest = plan.classes.last().expect("fifty-one classes");
    assert_eq!(deepest.path, "next.".repeat(49) + "next");
}

#[test]
fn a_declaration_collision_between_a_type_and_a_codec_is_refused() {
    // `Editor` and `EditorCodec` are both generated; a schema type actually
    // called `EditorCodec` would claim one of them.
    let error = Plan::build(&schema(
        "App",
        vec![
            def(
                "App",
                vec![FieldDef::new("e", Ty::Named("EditorCodec".to_string()))],
            ),
            def("EditorCodec", vec![FieldDef::new("zoom", Ty::Float)]),
            def("Editor", vec![FieldDef::new("zoom", Ty::Float)]),
        ],
    ))
    .expect_err("a collision");
    assert_eq!(
        error,
        EmitError::DeclarationCollision {
            name: "EditorCodec".to_string()
        }
    );
}

#[test]
fn a_key_the_path_parser_would_re_split_never_reaches_an_emitter() {
    // Belt and braces: `Schema::build` already refuses a key containing `.`,
    // and `is_emittable_key` refuses it again. Both are asserted because either
    // one alone would be a single point of failure for the property that makes
    // a path literal trustworthy.
    assert!(
        Schema::build(
            Ty::Named("App".to_string()),
            vec![def("App", vec![FieldDef::new("a.b", Ty::Int)])],
        )
        .is_err()
    );
    assert!(!crate::name::is_emittable_key("a.b"));
}

#[test]
fn every_minted_path_survives_the_real_parser() {
    // The property the whole compile-time-literal rule rests on, checked here
    // against `Path::parse` itself rather than against a re-derivation of its
    // grammar.
    let plan = Plan::build(&schema(
        "App",
        vec![
            def(
                "App",
                vec![FieldDef::new("editor", Ty::Named("Editor".to_string()))],
            ),
            def(
                "Editor",
                vec![
                    FieldDef::new("zoom", Ty::Float),
                    FieldDef::new("x2", Ty::Int),
                ],
            ),
        ],
    ))
    .expect("emittable");
    for class in &plan.classes {
        for accessor in &class.accessors {
            let parsed = Path::parse(&accessor.path).expect("a minted path parses");
            let keys: Vec<&str> = parsed
                .segments()
                .iter()
                .map(|s| match s {
                    Segment::Key(key) => key.as_str(),
                    Segment::Index(_) => panic!("a minted path has no index"),
                })
                .collect();
            assert_eq!(
                keys.last(),
                Some(&accessor.key.as_str()),
                "{} lost its last key",
                accessor.path
            );
            assert_eq!(
                parsed.to_string(),
                accessor.path,
                "the path did not round-trip"
            );
        }
    }
}
