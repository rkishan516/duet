//! Unit tests for the Dart emitter's two lookup tables — the type map and the
//! codec map — and for the shapes the goldens only show one instance of.
//!
//! The tables matter more than they look: mapping `int` to `double` would
//! produce a client that compiles, passes its own goldens, and reports a
//! mismatch on every read from a real host, because `Value::Int` and
//! `Value::Float` are distinct there and the runtime codecs refuse to widen one
//! into the other.

use super::*;

use duet_schema::{FieldDef, Schema, TypeDef};

use crate::emit::Options;

/// The Dart source for a schema with one field of type `ty`.
fn emitted(ty: Ty) -> String {
    let schema = Schema::build(
        Ty::Named("Root".to_string()),
        vec![
            TypeDef {
                name: "Root".to_string(),
                fields: vec![FieldDef::new("field", ty)],
            },
            TypeDef {
                name: "Leaf".to_string(),
                fields: vec![FieldDef::new("zoom", Ty::Float)],
            },
        ],
    )
    .expect("a legal schema");
    let plan = Plan::build(&schema).expect("emittable");
    emit(&plan, &Options::new("test", "test"))
}

#[test]
fn every_scalar_maps_to_the_dart_type_the_runtime_codec_produces() {
    // Each pair is checked against the codec's own type argument, not merely
    // asserted: `duetIntCodec` is a `DuetCodec<int>`, so `Ty::Int` must map to
    // `int` or the generated field would not compile.
    for (ty, dart, codec) in [
        (Ty::Bool, "bool", "duetBoolCodec"),
        (Ty::Int, "int", "duetIntCodec"),
        (Ty::Float, "double", "duetFloatCodec"),
        (Ty::Str, "String", "duetStringCodec"),
        (Ty::Bytes, "List<int>", "duetBytesCodec"),
        (Ty::Dynamic, "DuetValue", "duetDynamicCodec"),
    ] {
        assert_eq!(dart_inner_ty(&ty), dart, "{ty:?}");
        assert_eq!(dart_codec(&ty), codec, "{ty:?}");
        let source = emitted(ty.clone());
        assert!(
            source.contains(&format!("DuetField<{dart}> get field")),
            "{ty:?} did not produce a `{dart}` accessor"
        );
        assert!(
            source.contains(&format!("'field', {codec})")),
            "{ty:?} did not bind `{codec}`"
        );
    }
}

#[test]
fn containers_nest_their_type_and_their_codec_together() {
    assert_eq!(dart_inner_ty(&Ty::Int.list()), "List<int>");
    assert_eq!(
        dart_codec(&Ty::Int.list()),
        "duetListCodec<int>(duetIntCodec)"
    );
    assert_eq!(dart_inner_ty(&Ty::Str.map()), "Map<String, String>");
    assert_eq!(
        dart_codec(&Ty::Str.map()),
        "duetMapCodec<String>(duetStringCodec)"
    );
    assert_eq!(
        dart_inner_ty(&Ty::Float.list().map()),
        "Map<String, List<double>>"
    );
    assert_eq!(
        dart_codec(&Ty::Float.list().map()),
        "duetMapCodec<List<double>>(duetListCodec<double>(duetFloatCodec))"
    );
}

#[test]
fn a_named_type_uses_its_generated_const_codec() {
    assert_eq!(dart_inner_ty(&Ty::Named("Leaf".to_string())), "Leaf");
    assert_eq!(
        dart_codec(&Ty::Named("Leaf".to_string())),
        "const LeafCodec()"
    );
}

#[test]
fn an_optional_field_becomes_a_nullable_member_and_an_optional_handle() {
    let source = emitted(Ty::Str.optional());
    assert!(source.contains("final String? field;"), "{source}");
    assert!(
        source.contains("DuetOptionalField<String> get field"),
        "the handle is not optional"
    );
    assert!(
        source.contains("duetOptionalReading<String>(duetStringCodec"),
        "the decoder does not distinguish none from mismatch"
    );
    assert!(
        source.contains("field == null ? const DuetNull()"),
        "the encoder does not lower None to Null"
    );
}

#[test]
fn a_required_field_refuses_a_null_rather_than_accepting_it() {
    // `duetRequiredReading` reports a mismatch for a `Value::Null`, which is
    // what a required field promised a `T` should do. Using the optional reader
    // here would silently turn "another guest wrote null over my int" into
    // "the struct does not decode" — or worse, into a zero.
    let source = emitted(Ty::Int);
    assert!(source.contains("duetRequiredReading<int>(duetIntCodec"));
    assert!(!source.contains("duetOptionalReading"));
}

#[test]
fn an_empty_struct_still_emits_a_usable_class() {
    let schema = Schema::build(
        Ty::Named("Empty".to_string()),
        vec![TypeDef {
            name: "Empty".to_string(),
            fields: Vec::new(),
        }],
    )
    .expect("a legal schema");
    let source = emit(
        &Plan::build(&schema).expect("emittable"),
        &Options::new("t", "t"),
    );
    assert!(source.contains("const Empty();"), "{source}");
    assert!(source.contains("other is Empty;"), "{source}");
    assert!(source.contains("Object.hashAll(<Object?>[])"), "{source}");
    assert!(source.contains("return Empty();"), "{source}");
}

#[test]
fn a_short_field_list_stays_on_one_line_and_a_long_one_does_not() {
    // The wrapping is a function of the field *count*, not of a measured line
    // width, so a rename cannot silently rewrite a golden.
    let short = Schema::build(
        Ty::Named("Root".to_string()),
        vec![TypeDef {
            name: "Root".to_string(),
            fields: (0..INLINE_LIMIT)
                .map(|n| FieldDef::new(format!("f{n}"), Ty::Int))
                .collect(),
        }],
    )
    .expect("legal");
    let long = Schema::build(
        Ty::Named("Root".to_string()),
        vec![TypeDef {
            name: "Root".to_string(),
            fields: (0..INLINE_LIMIT + 1)
                .map(|n| FieldDef::new(format!("f{n}"), Ty::Int))
                .collect(),
        }],
    )
    .expect("legal");
    let options = Options::new("t", "t");
    assert!(
        emit(&Plan::build(&short).expect("emittable"), &options)
            .contains("const Root({required this.f0,")
    );
    assert!(
        emit(&Plan::build(&long).expect("emittable"), &options)
            .contains("const Root({\n    required this.f0,\n")
    );
}

#[test]
fn the_generated_docs_read_as_english_for_both_articles() {
    assert_eq!(an("App"), "an `App`");
    assert_eq!(an("Editor"), "an `Editor`");
    assert_eq!(an("Wide"), "a `Wide`");
    assert_eq!(an(""), "a ``");
}

#[test]
fn the_root_path_is_described_rather_than_shown_as_an_empty_literal() {
    assert_eq!(describe(""), "the store's root");
    assert_eq!(describe("editor.zoom"), "`editor.zoom`");
}
