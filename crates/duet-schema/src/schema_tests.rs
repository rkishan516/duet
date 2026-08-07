//! Tests for [`Schema`](crate::Schema): validation, depth, and the render.
//!
//! Split out of `schema.rs` under `#[path]` so both files stay small; they are
//! one module as far as visibility is concerned.

use super::*;
use crate::decode::DecodeError;
use crate::state::NotNullable;
use crate::ty::FieldDef;
use duet_core::Value;

/// Builds a `SharedState` impl whose only interesting part is its schema.
///
/// `to_value`/`from_value` are stubs: every test here is about the *schema*
/// half of the contract, and giving each fixture a real decoder would be
/// several hundred lines proving nothing these tests ask about. The impls in
/// `src/impls/` and the doc example on the crate root exercise the other two
/// methods.
macro_rules! schema_only {
    ($name:ident, $wire:literal, |$registry:ident| $body:expr) => {
        #[derive(Debug, PartialEq)]
        struct $name;

        impl SharedState for $name {
            fn to_value(&self) -> Value {
                Value::map([])
            }

            fn from_value(value: &Value) -> Result<Self, DecodeError> {
                match value {
                    Value::Map(_) => Ok($name),
                    other => Err(DecodeError::wrong_type($wire, other)),
                }
            }

            fn schema($registry: &mut Registry) -> Ty {
                $body
            }
        }

        impl NotNullable for $name {}
    };
}

schema_only!(Leaf, "Leaf", |registry| {
    registry.define::<Self>("Leaf", |_| vec![FieldDef::new("zoom", Ty::Float)])
});

schema_only!(Root, "Root", |registry| {
    registry.define::<Self>("Root", |r| {
        vec![
            FieldDef::new("counter", Ty::Int),
            FieldDef::new("leaf", Leaf::schema(r)),
            FieldDef::new("leaves", Leaf::schema(r).list()),
        ]
    })
});

schema_only!(Orphan, "Orphan", |registry| {
    registry.define::<Self>("Orphan", |_| vec![FieldDef::new("code", Ty::Str)])
});

schema_only!(SelfReferential, "SelfReferential", |registry| {
    registry.define::<Self>("Node", |r| {
        vec![FieldDef::new("next", SelfReferential::schema(r).optional())]
    })
});

schema_only!(BadName, "BadName", |registry| {
    registry.define::<Self>("2Bad", |_| vec![FieldDef::new("a", Ty::Int)])
});

schema_only!(BadKey, "BadKey", |registry| {
    registry.define::<Self>("BadKey", |_| vec![FieldDef::new("a.b", Ty::Int)])
});

schema_only!(EmptyKey, "EmptyKey", |registry| {
    registry.define::<Self>("EmptyKey", |_| vec![FieldDef::new("", Ty::Int)])
});

schema_only!(Dangling, "Dangling", |registry| {
    registry.define::<Self>("Dangling", |_| {
        vec![FieldDef::new("ghost", Ty::Named("Ghost".to_string()))]
    })
});

schema_only!(Empty, "Empty", |registry| {
    registry.define::<Self>("Empty", |_| Vec::new())
});

// --- Building and validating ---

#[test]
fn a_valid_schema_records_its_types_sorted_by_name() {
    let schema = Schema::of::<Root>().expect("Root is a valid schema");
    assert_eq!(schema.root(), &Ty::Named("Root".to_string()));
    assert_eq!(
        schema
            .types()
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>(),
        ["Leaf", "Root"]
    );
}

#[test]
fn a_type_reached_twice_is_defined_once() {
    let schema = Schema::of::<Root>().expect("Root is valid");
    assert_eq!(
        schema.types().iter().filter(|t| t.name == "Leaf").count(),
        1,
        "Leaf is reached through two fields and must be defined once"
    );
}

#[test]
fn a_primitive_root_needs_no_types() {
    let schema = Schema::of::<i64>().expect("a bare i64 is a legal root");
    assert_eq!(schema.root(), &Ty::Int);
    assert!(schema.types().is_empty());
    assert_eq!(schema.depth(), 0);
}

#[test]
fn field_order_is_declaration_order_not_alphabetical() {
    // Generated positional constructors depend on this, so it is part of the
    // contract rather than an incidental property of the walk.
    let schema = Schema::of::<Root>().expect("Root is valid");
    let root = schema
        .types()
        .iter()
        .find(|t| t.name == "Root")
        .expect("Root is defined");
    assert_eq!(
        root.fields
            .iter()
            .map(|f| f.key.as_str())
            .collect::<Vec<_>>(),
        ["counter", "leaf", "leaves"]
    );
}

// --- Cycle detection ---

#[test]
fn a_recursive_type_is_a_typed_error_not_a_stack_overflow() {
    let errors = Schema::of::<SelfReferential>().expect_err("a recursive type is not a schema");
    assert_eq!(errors.to_string(), "recursive type: Node -> Node");
    assert_eq!(
        errors.as_slice(),
        [SchemaError::Recursive {
            chain: vec!["Node".to_string(), "Node".to_string()],
        }]
    );
}

#[test]
fn a_cycle_is_reported_once_though_two_checks_find_it() {
    // `Registry::define` catches it as the schema is built and `resolve`
    // catches it in the finished graph. Both must run — they cover different
    // construction routes — and the developer must still see one message.
    let errors = Schema::of::<SelfReferential>().expect_err("recursive");
    assert_eq!(errors.as_slice().len(), 1, "{errors}");
}

#[test]
fn a_dangling_named_reference_is_rejected() {
    let errors = Schema::of::<Dangling>().expect_err("Ghost is not defined");
    assert_eq!(errors.to_string(), "no type named \"Ghost\" is defined");
}

// --- Names and keys ---

#[test]
fn an_illegal_type_name_is_rejected() {
    let errors = Schema::of::<BadName>().expect_err("2Bad is not an identifier");
    assert!(
        errors.as_slice().contains(&SchemaError::IllegalName {
            name: "2Bad".to_string()
        }),
        "{errors}"
    );
}

#[test]
fn a_key_that_would_not_round_trip_through_the_path_parser_is_rejected() {
    // The exact hazard `Path::from_segments` only catches with a debug_assert:
    // one segment in, two out on re-parse.
    let errors = Schema::of::<BadKey>().expect_err("a.b is two segments");
    assert_eq!(
        errors.as_slice(),
        [SchemaError::IllegalKey {
            type_name: "BadKey".to_string(),
            key: "a.b".to_string(),
        }]
    );
}

#[test]
fn an_empty_key_is_rejected() {
    let errors = Schema::of::<EmptyKey>().expect_err("the empty key is the root path");
    assert_eq!(
        errors.as_slice(),
        [SchemaError::IllegalKey {
            type_name: "EmptyKey".to_string(),
            key: String::new(),
        }]
    );
}

#[test]
fn every_key_of_every_valid_schema_round_trips_through_path_parse() {
    // The property the emitters rely on: a path literal built by joining keys
    // with `.` addresses exactly the node the schema describes.
    for def in Schema::of::<Root>().expect("Root is valid").types() {
        for field in &def.fields {
            let parsed = Path::parse(&field.key).expect("a validated key parses");
            assert_eq!(parsed.to_string(), field.key);
            assert_eq!(parsed.segments().len(), 1, "key {:?}", field.key);
        }
    }
}

#[test]
fn the_key_round_trip_predicate_rejects_what_the_parser_reinterprets() {
    assert!(key_round_trips("zoom"));
    assert!(key_round_trips("café"));
    for bad in ["", "a.b", "a[0]", "]", "[0]"] {
        assert!(!key_round_trips(bad), "{bad:?} must not round-trip");
    }
}

// --- Depth ---

#[test]
fn a_struct_counts_as_one_container_plus_its_deepest_field() {
    // Root { counter, leaf: Leaf { zoom }, leaves: [Leaf] }
    //   Leaf                   = 1
    //   Root.leaves = list(Leaf) = 1 + 1 = 2
    //   Root                   = 1 + 2 = 3
    assert_eq!(Schema::of::<Root>().expect("valid").depth(), 3);
    assert_eq!(Schema::of::<Leaf>().expect("valid").depth(), 1);
    assert_eq!(Schema::of::<Empty>().expect("valid").depth(), 1);
}

#[test]
fn an_option_adds_no_container_of_its_own() {
    // `None` is a scalar and `Some(x)` is exactly `x`.
    assert_eq!(Schema::of::<Option<i64>>().expect("valid").depth(), 0);
    assert_eq!(Schema::of::<Option<Vec<i64>>>().expect("valid").depth(), 1);
}

#[test]
fn lists_and_maps_each_add_one() {
    assert_eq!(Schema::of::<Vec<i64>>().expect("valid").depth(), 1);
    assert_eq!(Schema::of::<Vec<Vec<i64>>>().expect("valid").depth(), 2);
    assert_eq!(
        Schema::of::<std::collections::BTreeMap<String, Vec<i64>>>()
            .expect("valid")
            .depth(),
        2
    );
}

#[test]
fn the_declared_depth_matches_what_a_real_value_of_it_measures() {
    // The whole point of the bound: the schema's number and `Value::depth`'s
    // number have to be the same number, or the check guards nothing.
    let value = Root.to_value();
    assert_eq!(value.depth(), 1, "the stub `to_value` is an empty map");

    let realistic = Value::map([
        ("counter", Value::Int(0)),
        ("leaf", Value::map([("zoom", Value::Float(1.0))])),
        (
            "leaves",
            Value::List(vec![Value::map([("zoom", Value::Float(1.0))])]),
        ),
    ]);
    assert_eq!(
        realistic.depth(),
        Schema::of::<Root>().expect("valid").depth()
    );
}

#[test]
fn a_schema_deeper_than_the_store_accepts_is_rejected() {
    // A list nested MAX_VALUE_DEPTH + 1 deep. Built by `Ty` rather than by
    // Rust types, because expressing 62 nested `Vec`s as a type would be 62
    // lines of turbofish proving the same thing.
    let mut ty = Ty::Int;
    for _ in 0..=MAX_VALUE_DEPTH {
        ty = ty.list();
    }
    let errors = check_depth(&ty, &[], &[], &[]);
    assert_eq!(
        errors,
        [SchemaError::TooDeep {
            depth: MAX_VALUE_DEPTH + 1,
            max: MAX_VALUE_DEPTH,
        }]
    );

    // Exactly at the limit is accepted: an off-by-one here would silently cost
    // a whole level of nesting.
    let mut at_limit = Ty::Int;
    for _ in 0..MAX_VALUE_DEPTH {
        at_limit = at_limit.list();
    }
    assert!(check_depth(&at_limit, &[], &[], &[]).is_empty());
}

#[test]
fn dynamic_contributes_nothing_to_the_declared_depth() {
    // It cannot contribute anything honest: what a guest put there is not a
    // property of the schema. The store bounds it per write instead.
    assert_eq!(Schema::of::<Value>().expect("valid").depth(), 0);
    assert_eq!(Schema::of::<Vec<Value>>().expect("valid").depth(), 1);
}

// --- Rendering ---

#[test]
fn the_render_is_byte_stable_across_repeated_builds() {
    let first = Schema::of::<Root>().expect("valid").render();
    let second = Schema::of::<Root>().expect("valid").render();
    assert_eq!(first, second);
}

#[test]
fn the_render_ends_with_exactly_one_newline() {
    let rendered = Schema::of::<Root>().expect("valid").render();
    assert!(rendered.ends_with("}\n"));
    assert!(!rendered.ends_with("\n\n"));
}

#[test]
fn the_whole_document_is_pinned() {
    // A golden, inline. Increment 4 reads this format with an independent
    // `serde_json` reader, so any drift here is a contract change and should
    // read as one in a diff.
    let rendered = Schema::of::<Root>().expect("valid").render();
    assert_eq!(
        rendered,
        r#"{
  "commands": [],
  "root": {"kind": "named", "name": "Root"},
  "types": [
    {
      "fields": [
        {"key": "zoom", "type": {"kind": "float"}}
      ],
      "name": "Leaf"
    },
    {
      "fields": [
        {"key": "counter", "type": {"kind": "int"}},
        {"key": "leaf", "type": {"kind": "named", "name": "Leaf"}},
        {"key": "leaves", "type": {"kind": "list", "of": {"kind": "named", "name": "Leaf"}}}
      ],
      "name": "Root"
    }
  ],
  "version": 2
}
"#
    );
}

#[test]
fn a_schema_with_no_types_renders_an_empty_array() {
    assert_eq!(
        Schema::of::<i64>().expect("valid").render(),
        "{\n  \"commands\": [],\n  \"root\": {\"kind\": \"int\"},\n  \"types\": [],\n  \"version\": 2\n}\n"
    );
}

#[test]
fn a_type_with_no_fields_renders_an_empty_array() {
    let rendered = Schema::of::<Empty>().expect("valid").render();
    assert!(rendered.contains("\"fields\": [],"), "{rendered}");
}

#[test]
fn every_ty_arm_reaches_the_renderer_through_a_whole_document() {
    // Guards against an arm whose spelling nobody ever exercised. Each arm is
    // wrapped in all three containers so the wrappers are exercised against
    // every leaf rather than only against the one the fixture happens to use.
    let arms = [
        (Ty::Bool, "\"bool\""),
        (Ty::Int, "\"int\""),
        (Ty::Float, "\"float\""),
        (Ty::Str, "\"string\""),
        (Ty::Bytes, "\"bytes\""),
        (Ty::Dynamic, "\"dynamic\""),
        (Ty::Named("Leaf".to_string()), "\"named\""),
    ];
    for (arm, spelling) in arms {
        let rendered = Schema {
            root: arm.list().map().optional(),
            types: vec![TypeDef {
                name: "Leaf".to_string(),
                fields: vec![FieldDef::new("zoom", Ty::Float)],
            }],
            commands: Vec::new(),
        }
        .render();
        for expected in [spelling, "\"optional\"", "\"map\"", "\"list\""] {
            assert!(
                rendered.contains(expected),
                "{expected} missing: {rendered}"
            );
        }
    }
}

#[test]
fn build_validates_a_hand_assembled_schema_the_same_way_of_does() {
    let schema = Schema::build(
        Ty::Named("App".to_string()),
        vec![TypeDef {
            name: "App".to_string(),
            fields: vec![FieldDef::new("counter", Ty::Int)],
        }],
    )
    .expect("a legal schema");
    assert_eq!(schema.root(), &Ty::Named("App".to_string()));
    assert_eq!(schema.types().len(), 1);
}

#[test]
fn build_sorts_types_by_name_whatever_order_they_arrive_in() {
    // `render`'s byte-stability must not depend on how the caller assembled the
    // list, or a schema read back from JSON would re-render differently from
    // the file it was read from.
    let leaf = TypeDef {
        name: "Editor".to_string(),
        fields: vec![FieldDef::new("zoom", Ty::Float)],
    };
    let root = TypeDef {
        name: "App".to_string(),
        fields: vec![FieldDef::new("editor", Ty::Named("Editor".to_string()))],
    };
    let forwards = Schema::build(
        Ty::Named("App".to_string()),
        vec![root.clone(), leaf.clone()],
    )
    .expect("a legal schema");
    let backwards =
        Schema::build(Ty::Named("App".to_string()), vec![leaf, root]).expect("a legal schema");
    assert_eq!(forwards, backwards);
    assert_eq!(forwards.render(), backwards.render());
}

#[test]
fn build_rejects_two_definitions_sharing_a_name() {
    // `Registry` is keyed by name and cannot produce this; a parsed document
    // can, and `resolve`'s by-name map would silently keep only one.
    let errors = Schema::build(
        Ty::Named("App".to_string()),
        vec![
            TypeDef {
                name: "App".to_string(),
                fields: vec![FieldDef::new("counter", Ty::Int)],
            },
            TypeDef {
                name: "App".to_string(),
                fields: vec![FieldDef::new("title", Ty::Str)],
            },
        ],
    )
    .expect_err("two definitions on one name");
    assert!(
        errors.0.contains(&SchemaError::NameCollision {
            name: "App".to_string()
        }),
        "{errors}"
    );
}

#[test]
fn build_rejects_two_fields_on_one_key() {
    let errors = Schema::build(
        Ty::Named("App".to_string()),
        vec![TypeDef {
            name: "App".to_string(),
            fields: vec![
                FieldDef::new("zoom", Ty::Int),
                FieldDef::new("zoom", Ty::Str),
            ],
        }],
    )
    .expect_err("two fields on one key");
    assert_eq!(
        errors.0,
        vec![SchemaError::DuplicateKey {
            type_name: "App".to_string(),
            key: "zoom".to_string(),
        }]
    );
}

#[test]
fn build_rejects_a_cycle_a_registry_could_never_have_produced() {
    let errors = Schema::build(
        Ty::Named("Node".to_string()),
        vec![TypeDef {
            name: "Node".to_string(),
            fields: vec![FieldDef::new(
                "next",
                Ty::Named("Node".to_string()).optional(),
            )],
        }],
    )
    .expect_err("a self-referential type");
    assert_eq!(
        errors.0,
        vec![SchemaError::Recursive {
            chain: vec!["Node".to_string(), "Node".to_string()],
        }]
    );
}

#[test]
fn build_rejects_a_dangling_reference() {
    let errors = Schema::build(Ty::Named("Missing".to_string()), Vec::new())
        .expect_err("a reference to nothing");
    assert_eq!(
        errors.0,
        vec![SchemaError::UnknownType {
            name: "Missing".to_string()
        }]
    );
}

// --- Commands ---

/// A command with one `i64` parameter, returning an `i64`.
fn add() -> CommandDef {
    CommandDef::new(
        "add",
        vec![FieldDef::new("a", Ty::Int), FieldDef::new("b", Ty::Int)],
        Ty::Int,
    )
}

/// The schema of [`Root`] plus `commands`.
fn with_commands(commands: Vec<CommandDef>) -> Result<Schema, SchemaErrors> {
    Schema::of_with_commands::<Root>(|_| commands)
}

#[test]
fn a_schema_with_no_commands_still_renders_the_key() {
    // The decision `Schema::render` documents, pinned so a "tidier" writer that
    // omitted the empty array is a failing test rather than a silent format
    // change that only version detection would catch.
    let rendered = Schema::of::<Root>().expect("valid").render();
    assert!(rendered.contains("\"commands\": [],"), "{rendered}");
    assert!(Schema::of::<Root>().expect("valid").commands().is_empty());
}

#[test]
fn a_command_renders_with_its_parameters_in_declaration_order() {
    let rendered = with_commands(vec![add()]).expect("valid").render();
    assert_eq!(
        rendered,
        r#"{
  "commands": [
    {
      "name": "add",
      "params": [
        {"key": "a", "type": {"kind": "int"}},
        {"key": "b", "type": {"kind": "int"}}
      ],
      "returns": {"kind": "int"}
    }
  ],
  "root": {"kind": "named", "name": "Root"},
  "types": [
    {
      "fields": [
        {"key": "zoom", "type": {"kind": "float"}}
      ],
      "name": "Leaf"
    },
    {
      "fields": [
        {"key": "counter", "type": {"kind": "int"}},
        {"key": "leaf", "type": {"kind": "named", "name": "Leaf"}},
        {"key": "leaves", "type": {"kind": "list", "of": {"kind": "named", "name": "Leaf"}}}
      ],
      "name": "Root"
    }
  ],
  "version": 2
}
"#
    );
}

#[test]
fn each_of_the_four_return_shapes_has_its_own_spelling() {
    // The whole table on `CommandDef`, rendered. Absence is spelled by omitting
    // the key, so the assertion has to be about what is *not* there as much as
    // about what is.
    let cases: [(CommandDef, &str); 4] = [
        (
            CommandDef::new("plain", Vec::new(), Ty::Int),
            "{\n      \"name\": \"plain\",\n      \"params\": [],\n      \
             \"returns\": {\"kind\": \"int\"}\n    }",
        ),
        (
            CommandDef {
                name: "nothing".to_string(),
                params: Vec::new(),
                returns: None,
                raises: None,
            },
            "{\n      \"name\": \"nothing\",\n      \"params\": []\n    }",
        ),
        (
            CommandDef {
                name: "fallible".to_string(),
                params: Vec::new(),
                returns: Some(Ty::Int),
                raises: Some(Ty::Str),
            },
            "{\n      \"name\": \"fallible\",\n      \"params\": [],\n      \
             \"raises\": {\"kind\": \"string\"},\n      \
             \"returns\": {\"kind\": \"int\"}\n    }",
        ),
        (
            CommandDef {
                name: "fallible_void".to_string(),
                params: Vec::new(),
                returns: None,
                raises: Some(Ty::Str),
            },
            "{\n      \"name\": \"fallible_void\",\n      \"params\": [],\n      \
             \"raises\": {\"kind\": \"string\"}\n    }",
        ),
    ];
    for (command, expected) in cases {
        let name = command.name.clone();
        let rendered = with_commands(vec![command]).expect("valid").render();
        assert!(rendered.contains(expected), "{name}: {rendered}");
    }
}

#[test]
fn commands_are_rendered_sorted_by_name_however_they_were_registered() {
    // Byte-stability: reordering a `commands![…]` list must not move the file.
    let names = |commands: Vec<CommandDef>| -> Vec<String> {
        with_commands(commands)
            .expect("valid")
            .commands()
            .iter()
            .map(|c| c.name.clone())
            .collect()
    };
    let one = |name: &str| CommandDef::new(name, Vec::new(), Ty::Int);
    assert_eq!(
        names(vec![one("zebra"), one("apple"), one("mango")]),
        ["apple", "mango", "zebra"]
    );
    assert_eq!(
        names(vec![one("apple"), one("mango"), one("zebra")]),
        ["apple", "mango", "zebra"]
    );
}

#[test]
fn two_commands_on_one_name_are_rejected() {
    let errors = with_commands(vec![add(), add()]).expect_err("one name, two commands");
    assert_eq!(
        errors.0,
        vec![SchemaError::CommandCollision {
            name: "add".to_string()
        }]
    );
}

#[test]
fn an_illegal_command_name_is_rejected() {
    let errors = with_commands(vec![CommandDef::new("2fast", Vec::new(), Ty::Int)])
        .expect_err("not an identifier");
    assert_eq!(
        errors.0,
        vec![SchemaError::IllegalCommandName {
            name: "2fast".to_string()
        }]
    );
}

#[test]
fn a_parameter_key_follows_the_same_rule_a_field_key_does() {
    // One notion of a wire key, applied wherever the format admits one.
    for bad in ["", "a.b", "a[0]"] {
        let errors = with_commands(vec![CommandDef::new(
            "bump",
            vec![FieldDef::new(bad, Ty::Int)],
            Ty::Int,
        )])
        .expect_err("not a wire key");
        assert!(
            errors.0.contains(&SchemaError::IllegalParam {
                command: "bump".to_string(),
                key: bad.to_string(),
            }),
            "{bad}: {:?}",
            errors.0
        );
    }
}

#[test]
fn two_parameters_on_one_key_are_rejected() {
    let errors = with_commands(vec![CommandDef::new(
        "bump",
        vec![FieldDef::new("by", Ty::Int), FieldDef::new("by", Ty::Int)],
        Ty::Int,
    )])
    .expect_err("one key, two parameters");
    assert_eq!(
        errors.0,
        vec![SchemaError::DuplicateParam {
            command: "bump".to_string(),
            key: "by".to_string(),
        }]
    );
}

#[test]
fn a_command_may_name_a_type_the_root_never_reaches() {
    // The case that makes the walk over commands load-bearing: an error type is
    // very often reachable from nothing else in the schema.
    let schema = Schema::of_with_commands::<Root>(|registry| {
        vec![CommandDef {
            name: "save".to_string(),
            params: Vec::new(),
            returns: None,
            raises: Some(Orphan::schema(registry)),
        }]
    })
    .expect("the command's own type is defined by describing it");
    assert!(
        schema.types().iter().any(|t| t.name == "Orphan"),
        "the error type must be in `types`: {:?}",
        schema.types()
    );
}

#[test]
fn a_command_naming_a_type_nothing_defines_is_rejected() {
    // The same walk, from the other side: without commands as walk starts this
    // would build cleanly and emit a client with a dangling type.
    let errors = Schema::build_with_commands(
        Ty::Int,
        Vec::new(),
        vec![CommandDef::new(
            "save",
            vec![FieldDef::new("doc", Ty::Named("Ghost".to_string()))],
            Ty::Int,
        )],
    )
    .expect_err("a reference to nothing");
    assert_eq!(
        errors.0,
        vec![SchemaError::UnknownType {
            name: "Ghost".to_string()
        }]
    );
}

#[test]
fn a_command_type_deeper_than_the_store_accepts_is_rejected() {
    // Depth validation covers a command's parameters and its two reply types,
    // not only the root. Each position separately, because a guard on one of
    // the three is a guard that looks present and is not.
    let mut deep = Ty::Int;
    for _ in 0..=MAX_VALUE_DEPTH {
        deep = deep.list();
    }
    let positions: [(&str, CommandDef); 3] = [
        (
            "a parameter",
            CommandDef::new("f", vec![FieldDef::new("x", deep.clone())], Ty::Int),
        ),
        ("a return", CommandDef::new("f", Vec::new(), deep.clone())),
        (
            "an error",
            CommandDef {
                name: "f".to_string(),
                params: Vec::new(),
                returns: None,
                raises: Some(deep.clone()),
            },
        ),
    ];
    for (position, command) in positions {
        let errors =
            Schema::build_with_commands(Ty::Int, Vec::new(), vec![command]).expect_err(position);
        assert_eq!(
            errors.0,
            vec![SchemaError::TooDeep {
                depth: MAX_VALUE_DEPTH + 1,
                max: MAX_VALUE_DEPTH,
            }],
            "{position}"
        );
    }
}
