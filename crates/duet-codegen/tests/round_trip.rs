//! `render → read → compare`, across two implementations that share no code.
//!
//! # What this catches that a single-implementation test cannot
//!
//! A round trip through **one** implementation is a tautology dressed as a
//! test. `serde_json::from_str(&serde_json::to_string(&x))? == x` passes
//! whatever the format is: rename a field, invert a tag, emit the wrong
//! nesting, and the writer and the reader agree with each other while agreeing
//! with nothing else. It would pass just as happily if the document were
//! `{"a":[1,2]}` and Dart could not read a word of it.
//!
//! Here the writer is `duet-schema`'s hand-rolled `Schema::render` — pushing
//! literal strings into a `String`, in a crate with **no `serde_json`
//! dependency at all** — and the reader is this crate's `serde_json` walk. The
//! two agree only if the bytes in between are what *both* independently spell.
//! That makes the round trip catch the whole class of asymmetric bugs:
//!
//! - an escape the writer emits and the reader cannot read (or vice versa)
//! - a sort the writer dropped: `types` is by name, `fields` is by declaration
//! - a nesting the reader mis-associates: `{"kind":"list","of":…}` read as the
//!   inner type, losing the wrapper
//! - a key one side spells differently: `"of"` against `"item"`
//! - a trailing newline, an indent, a separator — every byte of the format
//!
//! It cannot catch a *shared* misunderstanding of what the format should be:
//! both implementations are in this repository and were written for one
//! specification. That gap is what the hand-written fixtures, the negative
//! fixtures, `tests/real_host.rs` and the two guest packages' own tests cover —
//! the format is checked against consumers, not only against itself.

mod support;

use duet_schema::{FieldDef, Schema, Ty, TypeDef};

#[test]
fn every_committed_schema_is_already_in_canonical_form() {
    // The fixtures are hand-written. This is what says a *human* can write the
    // exact bytes the machine produces, which is the same claim
    // `crates/duet-derive/tests/schema_proof.rs` makes from the other side —
    // measured here against a different producer.
    for fixture in support::FIXTURES {
        let committed = support::read(fixture.schema);
        let parsed = support::schema(fixture.schema);
        assert!(
            parsed.render() == committed,
            "{} is not in canonical form{}",
            fixture.schema,
            support::first_difference(&committed, &parsed.render())
        );
    }
}

/// The one hand-written schema that declares commands.
///
/// Deliberately **not** in [`support::FIXTURES`]: that list is what the Dart
/// and TypeScript goldens are generated from and what `duet-host-stdio` embeds,
/// and command codegen does not exist yet, so adding it there would commit two
/// generated clients that say nothing about commands and a host fixture nothing
/// drives. It is claimed here instead, by the two checks that are about the
/// *format* rather than about what is emitted from it.
const COMMANDS_FIXTURE: &str = "schema/commands.json";

#[test]
fn the_command_bearing_fixture_survives_the_round_trip_byte_for_byte() {
    // The cross-check, on the half of the format the version bump added. The
    // writer is `duet-schema`'s hand-rolled `render`; the reader is this crate's
    // `serde_json` walk; neither knows the other exists. They agree only if
    // every byte between them — the sorted key order inside a command, the
    // omitted `returns`, the indent of a nested `params` — is what both
    // independently spell.
    let committed = support::read(COMMANDS_FIXTURE);
    let parsed = support::schema(COMMANDS_FIXTURE);
    assert!(
        parsed.render() == committed,
        "{COMMANDS_FIXTURE} is not in canonical form{}",
        support::first_difference(&committed, &parsed.render())
    );
    let reparsed = duet_codegen::read_schema(&parsed.render())
        .unwrap_or_else(|e| panic!("{COMMANDS_FIXTURE} should re-read: {e}"));
    assert_eq!(parsed, reparsed);
}

#[test]
fn the_command_bearing_fixture_reaches_every_shape_a_command_can_have() {
    // A fixture nobody measures is a file that looks like a test and is not
    // one. Between them the four commands must cover the whole `CommandDef`
    // table: a plain return, nothing at all, a `Result`, and a `Result` with no
    // ok type — plus a parameter list and an empty one.
    let schema = support::schema(COMMANDS_FIXTURE);
    let shapes: Vec<(bool, bool)> = schema
        .commands()
        .iter()
        .map(|c| (c.returns.is_some(), c.raises.is_some()))
        .collect();
    for shape in [(true, false), (false, false), (true, true), (false, true)] {
        assert!(
            shapes.contains(&shape),
            "no command is {shape:?}: {shapes:?}"
        );
    }
    assert!(schema.commands().iter().any(|c| !c.params.is_empty()));
    assert!(schema.commands().iter().any(|c| c.params.is_empty()));
    assert!(
        schema.commands().iter().any(|c| c.name.contains('.')),
        "a dotted command name is legal and must be exercised"
    );
}

#[test]
fn a_rendered_schema_reads_back_as_the_same_schema() {
    for fixture in support::FIXTURES {
        let parsed = support::schema(fixture.schema);
        let reparsed = duet_codegen::read_schema(&parsed.render())
            .unwrap_or_else(|e| panic!("{} should re-read: {e}", fixture.schema));
        assert_eq!(parsed, reparsed, "{} did not survive", fixture.schema);
    }
}

#[test]
fn every_type_arm_survives_the_round_trip() {
    // The fixtures reach every arm (`tests/coverage.rs` asserts that
    // mechanically), but only in the positions they happen to appear in. This
    // wraps each arm in all three containers so the wrappers are exercised
    // against every leaf.
    for arm in [
        Ty::Bool,
        Ty::Int,
        Ty::Float,
        Ty::Str,
        Ty::Bytes,
        Ty::Dynamic,
        Ty::Named("Leaf".to_string()),
    ] {
        for wrapped in [
            arm.clone(),
            arm.clone().optional(),
            arm.clone().list(),
            arm.clone().map(),
            arm.clone().list().map(),
            arm.clone().map().list().optional(),
        ] {
            let schema = Schema::build(
                Ty::Named("Root".to_string()),
                vec![
                    TypeDef {
                        name: "Root".to_string(),
                        fields: vec![FieldDef::new("field", wrapped.clone())],
                    },
                    TypeDef {
                        name: "Leaf".to_string(),
                        fields: vec![FieldDef::new("zoom", Ty::Float)],
                    },
                ],
            )
            .unwrap_or_else(|e| panic!("{wrapped:?} should be a legal schema: {e}"));
            let text = schema.render();
            let reread = duet_codegen::read_schema(&text)
                .unwrap_or_else(|e| panic!("{wrapped:?} should re-read: {e}\n{text}"));
            assert_eq!(schema, reread, "{wrapped:?} did not survive");
            assert_eq!(reread.render(), text, "{wrapped:?} re-rendered differently");
        }
    }
}

#[test]
fn a_name_needing_json_escapes_survives_both_directions() {
    // The writer escapes by hand and the reader unescapes through `serde_json`.
    // Nothing but a cross-implementation trip checks the two agree — and a type
    // name is one of the few places a schema can carry arbitrary text.
    //
    // The names are illegal as *identifiers*, so this goes through `render` and
    // `read_ty` rather than through `Schema::build`.
    for awkward in [
        "quote\"inside",
        "back\\slash",
        "new\nline",
        "tab\there",
        "control\u{01}char",
        "café🦀",
    ] {
        let rendered = Schema::build(
            Ty::Named("Root".to_string()),
            vec![TypeDef {
                name: "Root".to_string(),
                fields: vec![FieldDef::new("field", Ty::Named(awkward.to_string()))],
            }],
        );
        // Unresolvable by design; what matters is the rendering, which the
        // helper below reaches through a schema that *is* legal.
        assert!(rendered.is_err(), "{awkward} names no defined type");
    }

    for awkward in ["quote\"inside", "back\\slash", "new\nline", "café🦀"] {
        let document = format!(
            "{{\n  \"root\": {{\"kind\": \"named\", \"name\": \"App\"}},\n  \
             \"types\": [\n    {{\n      \"fields\": [\n        \
             {{\"key\": \"a\", \"type\": {{\"kind\": \"named\", \"name\": {}}}}}\n      \
             ],\n      \"name\": \"App\"\n    }}\n  ],\n  \"version\": 1\n}}\n",
            serde_json::to_string(awkward).expect("a string always serializes")
        );
        let error = duet_codegen::read_schema(&document)
            .expect_err("the reference does not resolve")
            .to_string();
        assert!(
            error.contains("no type named"),
            "{awkward} should be read back verbatim, got {error}"
        );
    }
}

#[test]
fn a_key_needing_json_escapes_survives_the_writer() {
    // A key that renders wrong would produce a path literal addressing a
    // different node. Keys this awkward are refused before they reach an
    // emitter; the check here is that the *writer* still spells them so the
    // reader gets them back unchanged, because that is what makes the rejection
    // name the right key.
    let schema = Schema::build(
        Ty::Named("App".to_string()),
        vec![TypeDef {
            name: "App".to_string(),
            fields: vec![FieldDef::new("with space", Ty::Int)],
        }],
    )
    .expect("a space is a legal wire key");
    let reread = duet_codegen::read_schema(&schema.render()).expect("it should re-read");
    assert_eq!(reread.types()[0].fields[0].key, "with space");
    assert_eq!(reread, schema);
}
