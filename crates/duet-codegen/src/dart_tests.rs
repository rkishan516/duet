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

/// One command, spelled compactly.
fn command(
    name: &str,
    params: Vec<FieldDef>,
    returns: Option<Ty>,
    raises: Option<Ty>,
) -> duet_schema::CommandDef {
    duet_schema::CommandDef {
        name: name.to_string(),
        params,
        returns,
        raises,
    }
}

/// The Dart source for a schema declaring `commands`.
fn with_commands(commands: Vec<duet_schema::CommandDef>) -> String {
    let schema = Schema::build_with_commands(
        Ty::Named("Root".to_string()),
        vec![
            TypeDef {
                name: "Root".to_string(),
                fields: vec![FieldDef::new("count", Ty::Int)],
            },
            TypeDef {
                name: "Leaf".to_string(),
                fields: vec![FieldDef::new("zoom", Ty::Float)],
            },
        ],
        commands,
    )
    .expect("a legal schema");
    emit(
        &Plan::build(&schema).expect("emittable"),
        &Options::new("test", "test"),
    )
}

#[test]
fn a_command_class_is_emitted_only_when_the_schema_declares_commands() {
    // The whole compatibility promise of this increment: a schema with no
    // commands must generate byte-for-byte what it generated before commands
    // existed, which means not so much as a blank line.
    let without = with_commands(Vec::new());
    assert!(
        !without.contains("Commands"),
        "a command-free schema must emit no commands class:\n{without}"
    );
    let with = with_commands(vec![command("ping", Vec::new(), None, None)]);
    assert!(with.contains("final class RootCommands {"), "{with}");
    assert!(
        with.starts_with(&without[..without.len() - 1]),
        "the commands class must be appended, leaving everything before it \
         untouched"
    );
}

#[test]
fn a_command_name_reaches_the_wire_verbatim_and_the_method_camel_cased() {
    // The mutation that has no error anywhere: `documentsClose` invoked against
    // a host that owns `documents.close` is refused at the far end of a call
    // the developer believed was typed. Both halves are asserted, because
    // asserting only the method name would pass for an emitter that camel-cased
    // both.
    let source = with_commands(vec![command("documents.close", Vec::new(), None, None)]);
    assert!(
        source.contains("client.invoke('documents.close')"),
        "the wire name must be the schema's own:\n{source}"
    );
    assert!(
        source.contains("documentsClose()"),
        "the method name must be camel-cased:\n{source}"
    );
    assert!(
        !source.contains("invoke('documentsClose')"),
        "the camel-cased name must never reach the wire:\n{source}"
    );
}

#[test]
fn argument_keys_are_literals_in_wire_order_and_the_signature_is_in_declaration_order() {
    let source = with_commands(vec![command(
        "bump",
        vec![FieldDef::new("path", Ty::Str), FieldDef::new("by", Ty::Int)],
        Some(Ty::Int),
        None,
    )]);
    assert!(
        source.contains("bump({required String path, required int by})"),
        "the signature keeps declaration order:\n{source}"
    );
    let args = source
        .split("client.invoke('bump'")
        .nth(1)
        .expect("the invocation");
    assert!(
        args.find("'by':").expect("by") < args.find("'path':").expect("path"),
        "the args literal must be in wire-key order:\n{source}"
    );
}

#[test]
fn a_command_binds_the_codecs_its_schema_types_call_for() {
    let source = with_commands(vec![command(
        "bump",
        Vec::new(),
        Some(Ty::Int),
        Some(Ty::Named("Leaf".to_string())),
    )]);
    assert!(
        source.contains("Future<DuetOutcome<int, Leaf>> bump()"),
        "{source}"
    );
    assert!(
        source.contains("        duetIntCodec,\n        const LeafCodec(),\n"),
        "the return and error codecs must be bound in that order:\n{source}"
    );
}

#[test]
fn a_command_with_no_declared_types_binds_the_dynamic_codec_on_both_arms() {
    // A command still answers when its schema declares nothing: the wire sends
    // null, and `duetDynamicCodec` is the identity that reads it.
    let source = with_commands(vec![command("ping", Vec::new(), None, None)]);
    assert!(
        source.contains("Future<DuetOutcome<DuetValue, DuetValue>> ping()"),
        "{source}"
    );
    assert!(
        source.contains("        duetDynamicCodec,\n        duetDynamicCodec,\n"),
        "{source}"
    );
}
