//! Unit tests for the TypeScript emitter's type and codec tables, and for the
//! import block — which is the one place this emitter can produce something the
//! Dart one cannot: a file that does not compile because it imported a name it
//! never used, or used a name it never imported.

use super::*;

use duet_schema::{FieldDef, Schema, TypeDef};

use crate::emit::Options;

/// The TypeScript source for a schema with one field of type `ty`.
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
    emit(
        &Plan::build(&schema).expect("emittable"),
        &Options::new("test", "test"),
    )
}

/// The inner type and codec expression for `ty`, with the imports discarded.
fn mapped(ty: &Ty) -> (String, String) {
    let mut body = Body::default();
    let inner = ts_inner_ty(&mut body, ty);
    let codec = ts_codec(&mut body, ty);
    (inner, codec)
}

#[test]
fn every_scalar_maps_to_the_typescript_type_the_runtime_codec_produces() {
    for (ty, expected, codec) in [
        (Ty::Bool, "boolean", "duetBoolCodec"),
        // `bigint`, not `number`: the wire's integer domain is `i64`, and a
        // `number`-backed field reads 9007199254740993 as ...992 and re-emits
        // it wrong with no error anywhere.
        (Ty::Int, "bigint", "duetIntCodec"),
        (Ty::Float, "number", "duetFloatCodec"),
        (Ty::Str, "string", "duetStringCodec"),
        (Ty::Bytes, "Uint8Array", "duetBytesCodec"),
        (Ty::Dynamic, "DuetValue", "duetDynamicCodec"),
    ] {
        assert_eq!(
            mapped(&ty),
            (expected.to_string(), codec.to_string()),
            "{ty:?}"
        );
        let source = emitted(ty.clone());
        assert!(
            source.contains(&format!("get field(): DuetField<{expected}>")),
            "{ty:?} did not produce a `{expected}` accessor"
        );
    }
}

#[test]
fn containers_nest_their_type_and_their_codec_together() {
    assert_eq!(
        mapped(&Ty::Int.list()),
        (
            "bigint[]".to_string(),
            "duetListCodec<bigint>(duetIntCodec)".to_string()
        )
    );
    assert_eq!(
        mapped(&Ty::Str.map()),
        (
            "Map<string, string>".to_string(),
            "duetMapCodec<string>(duetStringCodec)".to_string()
        )
    );
    assert_eq!(
        mapped(&Ty::Float.list().map()),
        (
            "Map<string, number[]>".to_string(),
            "duetMapCodec<number[]>(duetListCodec<number>(duetFloatCodec))".to_string()
        )
    );
}

#[test]
fn a_named_type_uses_its_generated_codec_constant() {
    assert_eq!(
        mapped(&Ty::Named("Leaf".to_string())),
        ("Leaf".to_string(), "leafCodec".to_string())
    );
    // A multi-word type name camel-cases the same way a field does.
    assert_eq!(codec_constant("AppState"), "appStateCodec");
}

#[test]
fn an_optional_field_becomes_a_nullable_member_and_an_optional_handle() {
    let source = emitted(Ty::Str.optional());
    assert!(
        source.contains("readonly field: string | null;"),
        "{source}"
    );
    assert!(source.contains("get field(): DuetOptionalField<string>"));
    assert!(source.contains("duetOptionalReading("));
    assert!(source.contains("value.field === null ? duetNull()"));
}

#[test]
fn the_imports_name_exactly_what_the_body_used() {
    // `packages/duet-js` compiles with `noUnusedLocals`, so an over-broad
    // import block is a *build failure* in the generated file rather than a
    // lint. Both directions are checked here because the emitter is the only
    // thing that knows, and neither direction is visible in a golden diff until
    // `tsc` runs.
    let source = emitted(Ty::Int);
    let imports = source
        .split("/** `Root`")
        .next()
        .expect("a header and an import block");

    for used in [
        "duetIntCodec",
        "duetRequiredReading",
        "DuetField",
        "duetMap",
    ] {
        assert!(imports.contains(used), "{used} is used but not imported");
    }
    for unused in [
        "duetOptionalReading",
        "duetReadingValue",
        "DuetOptionalField",
        "duetNull",
        "duetListCodec",
        "duetMapCodec",
        "duetBoolCodec",
        "duetDynamicCodec",
    ] {
        assert!(
            !imports.contains(unused),
            "{unused} is imported and never used"
        );
    }
}

#[test]
fn an_optional_field_pulls_in_the_symbols_only_it_needs() {
    // One type and one field, so nothing else in the schema can be the reason a
    // symbol was imported.
    let schema = Schema::build(
        Ty::Named("Root".to_string()),
        vec![TypeDef {
            name: "Root".to_string(),
            fields: vec![FieldDef::new("field", Ty::Str.optional())],
        }],
    )
    .expect("a legal schema");
    let source = emit(
        &Plan::build(&schema).expect("emittable"),
        &Options::new("t", "t"),
    );
    let imports = source
        .split("/** `Root`")
        .next()
        .expect("a header and an import block");
    for used in [
        "duetOptionalReading",
        "duetReadingValue",
        "DuetOptionalField",
        "duetNull",
    ] {
        assert!(imports.contains(used), "{used} is used but not imported");
    }
    assert!(
        !imports.contains("duetRequiredReading"),
        "a schema with no required field imported the required reader"
    );
}

#[test]
fn type_only_imports_are_spelled_as_type_imports() {
    // `verbatimModuleSyntax` is on: a type imported as a value survives into
    // the emitted JavaScript as an import of something that does not exist at
    // runtime.
    let source = emitted(Ty::Int);
    assert!(source.contains("type DuetValue,"));
    assert!(source.contains("type DuetCodec,"));
    assert!(source.contains("type DuetRouter,"));
    // `DuetField` is constructed, so it is a value import, not a type one.
    assert!(source.contains("\n  DuetField,\n"));
    assert!(!source.contains("type DuetField,"));
}

#[test]
fn a_module_that_needs_nothing_from_a_source_omits_that_import_entirely() {
    // An empty `import {} from '…'` is legal and pointless; more to the point,
    // an import block built from an empty set would emit one on every file.
    let mut body = Body::default();
    body.push("// nothing\n");
    assert_eq!(imports(&body, &Options::new("t", "t")), "");
}

#[test]
fn a_short_field_list_stays_on_one_line_and_a_long_one_does_not() {
    assert_eq!(listed(&["a: 1".to_string()], "  "), "{ a: 1 }");
    let many: Vec<String> = (0..INLINE_LIMIT + 1)
        .map(|n| format!("f{n}: {n}"))
        .collect();
    assert!(listed(&many, "    ").starts_with("{\n      f0: 0,\n"));
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

/// The TypeScript source for a schema declaring `commands`.
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
    // Including the import block, which this emitter writes from what the body
    // used: a commands class pulls in `DuetClient` and `duetDecodeOutcome`, and
    // a command-free schema must import neither.
    let without = with_commands(Vec::new());
    assert!(!without.contains("Commands"), "{without}");
    assert!(!without.contains("DuetClient"), "{without}");
    assert!(!without.contains("duetDecodeOutcome"), "{without}");

    let with = with_commands(vec![command("ping", Vec::new(), None, None)]);
    assert!(with.contains("export class RootCommands {"), "{with}");
    assert!(with.contains("  type DuetClient,\n"), "{with}");
    assert!(with.contains("  duetDecodeOutcome,\n"), "{with}");
    assert!(with.contains("  type DuetOutcome,\n"), "{with}");
}

#[test]
fn a_command_name_reaches_the_wire_verbatim_and_the_method_camel_cased() {
    let source = with_commands(vec![command("documents.close", Vec::new(), None, None)]);
    assert!(
        source.contains("this.client.invoke('documents.close')"),
        "{source}"
    );
    assert!(source.contains("async documentsClose()"), "{source}");
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
    let signature = source.split("async bump(").nth(1).expect("the signature");
    assert!(
        signature.find("path: string").expect("path") < signature.find("by: bigint").expect("by"),
        "the signature keeps declaration order:\n{source}"
    );
    // Split on the invocation rather than on the `Map` literal: the struct
    // encoder above builds one of those too, and a scanner that found it
    // instead would compare the wrong keys and pass.
    let args = source
        .split("this.client.invoke(")
        .nth(1)
        .expect("the args literal");
    assert!(
        args.find("['by',").expect("by") < args.find("['path',").expect("path"),
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
        source.contains("Promise<DuetOutcome<bigint, Leaf>>"),
        "{source}"
    );
    assert!(
        source.contains("      duetIntCodec,\n      leafCodec,\n"),
        "the return and error codecs must be bound in that order:\n{source}"
    );
}

#[test]
fn a_command_with_no_declared_types_binds_the_dynamic_codec_on_both_arms() {
    let source = with_commands(vec![command("ping", Vec::new(), None, None)]);
    assert!(
        source.contains("Promise<DuetOutcome<DuetValue, DuetValue>>"),
        "{source}"
    );
    assert!(
        source.contains("      duetDynamicCodec,\n      duetDynamicCodec,\n"),
        "{source}"
    );
}
