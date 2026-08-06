//! The one decision this increment has to get right: **a wire key is never
//! rewritten; an accessor name always is.**
//!
//! A Rust `snake_case` field becomes a `lowerCamelCase` accessor in Dart and in
//! TypeScript, because that is what a developer in either language expects to
//! type. The path it addresses keeps the schema's own spelling.
//!
//! # Why the asymmetry, and what breaks without it
//!
//! The path is the wire. It is a key in a `Value::Map` on the host, and it is
//! what a *second* guest uses to address the same node. Camel-case it and a
//! Dart guest writes `editor.fontSize` while the Rust host owns
//! `editor.font_size`: two names for what everyone believes is one field. There
//! is no error — `Value::Map` takes any key at its final segment — so both
//! writes succeed, both reads succeed, and each guest sees only its own. The
//! two-guest proof in `crates/duet-backend-macos/examples/two_guests.rs` has a
//! webview and a Flutter engine writing one store at the same time, which is
//! exactly the situation this would break silently.
//!
//! The tests below pin the rule from both directions, and the strongest of them
//! is not here: `tests/real_host.rs` resolves every path literal in the
//! committed goldens against a real `duet_core::Store`, so a camel-cased path
//! fails against the host rather than against an assertion about the host.

mod support;

use duet_codegen::{Options, generate, name, read_schema};

/// A schema whose every key is `snake_case`, including a nested struct.
const SNAKE: &str = r#"{
  "root": {"kind": "named", "name": "AppState"},
  "types": [
    {
      "fields": [
        {"key": "font_size", "type": {"kind": "int"}},
        {"key": "editor_state", "type": {"kind": "named", "name": "EditorState"}}
      ],
      "name": "AppState"
    },
    {
      "fields": [
        {"key": "zoom_level", "type": {"kind": "float"}},
        {"key": "is_dirty", "type": {"kind": "bool"}}
      ],
      "name": "EditorState"
    }
  ],
  "version": 1
}
"#;

#[test]
fn the_accessor_is_camel_cased_and_the_path_is_not() {
    let schema = read_schema(SNAKE).expect("a valid schema");
    let generated = generate(&schema, &Options::new("test", "test")).expect("emittable");

    for source in [&generated.dart, &generated.ts] {
        // The accessors are camel-cased…
        assert!(source.contains("get fontSize"), "no `fontSize` accessor");
        assert!(
            source.contains("get editorState"),
            "no `editorState` accessor"
        );
        assert!(source.contains("get zoomLevel"), "no `zoomLevel` accessor");
        assert!(source.contains("get isDirty"), "no `isDirty` accessor");

        // …and the paths are the schema's own keys, at every depth.
        assert!(source.contains("'font_size'"), "the path was rewritten");
        assert!(
            source.contains("'editor_state.zoom_level'"),
            "the nested path was rewritten"
        );
        assert!(
            source.contains("'editor_state.is_dirty'"),
            "the nested path was rewritten"
        );

        // The failing spelling, stated so the assertion cannot pass vacuously.
        assert!(
            !source.contains("'fontSize'"),
            "a camel-cased path reached the output"
        );
        assert!(
            !source.contains("'editorState"),
            "a camel-cased path segment reached the output"
        );
    }
}

#[test]
fn the_encoder_and_the_decoder_use_the_wire_key_too() {
    // The paths are only half of it. A struct's codec writes and reads map
    // entries by key, and a camel-cased key there would produce a `Value::Map`
    // the host cannot read fields out of — a different failure with the same
    // silence.
    let schema = read_schema(SNAKE).expect("a valid schema");
    let generated = generate(&schema, &Options::new("test", "test")).expect("emittable");

    assert!(generated.dart.contains("'font_size': duetIntCodec.encode("));
    assert!(generated.dart.contains("value.entries['font_size']"));
    assert!(generated.ts.contains("['font_size', duetIntCodec.encode("));
    assert!(generated.ts.contains("value.entries.get('font_size')"));

    assert!(!generated.dart.contains("'fontSize':"));
    assert!(!generated.ts.contains("['fontSize',"));
}

#[test]
fn the_committed_goldens_carry_a_snake_case_key_so_this_is_not_hypothetical() {
    // `schema/wide.json` has `snake_case_field` for exactly this reason. If the
    // fixtures were all single-word keys, every casing rule — including no rule
    // at all — would produce identical output and this whole file would be
    // asserting nothing.
    let wide = support::read("packages/duet/test/generated/wide.duet.dart");
    assert!(
        wide.contains("get snakeCaseField"),
        "the accessor is not camel"
    );
    assert!(
        wide.contains("'snake_case_field'"),
        "the path is not the key"
    );
    assert!(
        !wide.contains("'snakeCaseField'"),
        "the path was camel-cased"
    );

    let wide_ts = support::read("packages/duet-js/test/generated/wide.duet.ts");
    assert!(wide_ts.contains("get snakeCaseField()"));
    assert!(wide_ts.contains("'snake_case_field'"));
    assert!(!wide_ts.contains("'snakeCaseField'"));
}

#[test]
fn a_key_that_is_already_camel_is_left_alone_in_both_places() {
    // Not every schema comes from Rust. A key spelled `fontSize` is a legal
    // wire key, and camel-casing it must be idempotent rather than mangling.
    assert_eq!(name::lower_camel("fontSize"), "fontSize");
    let schema = read_schema(
        r#"{"root": {"kind": "named", "name": "App"}, "types": [
             {"fields": [{"key": "fontSize", "type": {"kind": "int"}}], "name": "App"}
           ], "version": 1}"#,
    )
    .expect("a valid schema");
    let generated = generate(&schema, &Options::new("test", "test")).expect("emittable");
    assert!(generated.dart.contains("get fontSize"));
    assert!(generated.dart.contains("'fontSize'"));
}

#[test]
fn two_keys_that_camel_case_alike_are_refused_rather_than_merged() {
    // `font_size` and `fontSize` are different wire keys and one Dart
    // identifier. Nothing here can know which the developer meant, and picking
    // one would make the other field unreachable from generated code while
    // still existing on the host.
    let schema = read_schema(&support::read("schema/unemittable/accessor_collision.json"))
        .expect("a valid schema");
    let error = generate(&schema, &Options::new("test", "test")).expect_err("a collision");
    assert!(error.to_string().contains("fontSize"), "{error}");
    assert!(error.to_string().contains("rename one"), "{error}");
}
