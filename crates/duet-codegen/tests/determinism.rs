//! Identical input, byte-identical output — including when "identical input"
//! arrived in a different order.
//!
//! `tests/goldens.rs` checks the weak form: generate twice, compare. That
//! catches a hash map's iteration order leaking into the output only if the map
//! happens to iterate differently between two runs in one process, which
//! `std::collections::HashMap` does **not** do — its randomness is per-process,
//! not per-iteration. So the weak form would sit there green over a genuine
//! nondeterminism.
//!
//! These reach it directly: the same schema assembled in a different order, and
//! the same schema written with different whitespace, must produce the same
//! bytes.

mod support;

use duet_codegen::{Options, generate, read_schema};
use duet_schema::{FieldDef, Schema, Ty, TypeDef};

/// The two definitions every case here is built from.
fn definitions() -> (TypeDef, TypeDef) {
    (
        TypeDef {
            name: "App".to_string(),
            fields: vec![
                FieldDef::new("counter", Ty::Int),
                FieldDef::new("editor", Ty::Named("Editor".to_string())),
                FieldDef::new("title", Ty::Str),
            ],
        },
        TypeDef {
            name: "Editor".to_string(),
            fields: vec![
                FieldDef::new("zoom", Ty::Float),
                FieldDef::new("theme", Ty::Str),
            ],
        },
    )
}

#[test]
fn the_order_definitions_are_assembled_in_does_not_reach_the_output() {
    let (app, editor) = definitions();
    let root = Ty::Named("App".to_string());
    let forwards =
        Schema::build(root.clone(), vec![app.clone(), editor.clone()]).expect("a legal schema");
    let backwards = Schema::build(root, vec![editor, app]).expect("a legal schema");

    let options = Options::new("test", "test");
    assert_eq!(
        generate(&forwards, &options).expect("emittable"),
        generate(&backwards, &options).expect("emittable"),
        "the assembly order reached the generated source"
    );
}

#[test]
fn the_whitespace_of_the_document_does_not_reach_the_output() {
    // The reader is `serde_json`, which does not preserve formatting; this pins
    // that nothing downstream reintroduces it — a header line, an indent
    // computed from the input, a byte count.
    let pretty = support::read("schema/app.json");
    let dense: String = pretty.lines().map(str::trim).collect::<Vec<_>>().join("");
    let options = Options::new("schema/app.json", "test");
    assert_eq!(
        generate(&read_schema(&pretty).expect("valid"), &options).expect("emittable"),
        generate(&read_schema(&dense).expect("valid"), &options).expect("emittable"),
    );
}

#[test]
fn field_order_does_reach_the_output_because_it_is_part_of_the_contract() {
    // The other half of the claim, and the reason `types` is sorted while
    // `fields` is not: reordering a struct's fields is a **source-breaking**
    // change for a generated client — the data class's members, its `toString`
    // and its `==` all follow declaration order — so it must be a deliberate
    // edit to the type rather than an emergent property of a map's iteration.
    let (app, editor) = definitions();
    let mut reordered = app.clone();
    reordered.fields.reverse();
    let root = Ty::Named("App".to_string());

    let options = Options::new("test", "test");
    let declared = generate(
        &Schema::build(root.clone(), vec![app, editor.clone()]).expect("legal"),
        &options,
    )
    .expect("emittable");
    let flipped = generate(
        &Schema::build(root, vec![reordered, editor]).expect("legal"),
        &options,
    )
    .expect("emittable");
    assert_ne!(
        declared, flipped,
        "field order must be visible in the output, or it is not a contract"
    );
}

#[test]
fn the_two_languages_agree_on_every_accessor_name() {
    // One naming rule, not two. A Dart client and a TypeScript client generated
    // from one schema must expose the same names, or the schema means different
    // things in the two guests — and the first person to notice would be
    // someone porting code between them.
    //
    // The expectation comes from the `Plan`, which is the single place either
    // emitter is allowed to learn a name; both artifacts are then checked
    // against it.
    for fixture in support::FIXTURES {
        let plan = duet_codegen::Plan::build(&support::schema(fixture.schema))
            .expect("the fixtures are emittable");
        let generated = fixture.generate();
        for class in &plan.classes {
            for name in std::iter::once("self".to_string())
                .chain(class.accessors.iter().map(|a| a.accessor.clone()))
            {
                assert!(
                    generated.dart.contains(&format!(" get {name} =>")),
                    "{}: Dart is missing the accessor `{name}`",
                    fixture.stem
                );
                assert!(
                    generated.ts.contains(&format!("  get {name}(): ")),
                    "{}: TypeScript is missing the accessor `{name}`",
                    fixture.stem
                );
            }
        }
    }
}
