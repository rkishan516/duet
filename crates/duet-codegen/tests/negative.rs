//! The schemas that must be refused, and the exact reason each one is refused
//! for.
//!
//! Two directories, because there are two different kinds of "no":
//!
//! - `schema/negative/` — documents that are **not a schema**. Malformed JSON,
//!   the wrong shapes, unknown kinds, and schemas whose *content* the validator
//!   rejects. These are what a hostile or hand-corrupted file looks like, and
//!   the reader has to survive every one of them.
//! - `schema/unemittable/` — documents that are perfectly valid schemas, but
//!   have no faithful spelling in Dart and TypeScript. These are a developer's
//!   type definitions being wrong for the target languages, not a broken file,
//!   and they are refused one layer later with a message naming the field.
//!
//! Both directories are walked whole: a fixture nobody asserts on would be a
//! file that looks like a test and is not one, so every file must be claimed by
//! an expectation below and every expectation must have a file.

mod support;

use std::collections::BTreeSet;

use duet_codegen::{EmitError, Options, ReadError, generate, read_schema};

/// One rejected fixture: its file stem, and a fragment its message must carry.
struct Rejection {
    stem: &'static str,
    message: &'static str,
}

/// Documents the reader must refuse, and what it must say about each.
///
/// The message fragment is asserted, not just the fact of a rejection: a reader
/// that answered "the schema is not valid JSON" to everything would pass a test
/// that only checked for `Err`, and would be useless to whoever has to fix the
/// file.
const REFUSED: &[Rejection] = &[
    Rejection {
        stem: "not_json",
        message: "not valid JSON",
    },
    Rejection {
        stem: "root_not_an_object",
        message: "the schema should be an object",
    },
    Rejection {
        stem: "bad_version",
        message: "declares version 99",
    },
    Rejection {
        stem: "version_not_a_number",
        message: "\"version\" should be a non-negative integer",
    },
    Rejection {
        stem: "unexpected_document_key",
        message: "\"notes\"",
    },
    Rejection {
        stem: "missing_types",
        message: "the schema has no \"types\"",
    },
    Rejection {
        stem: "types_not_an_array",
        message: "\"types\" should be an array",
    },
    Rejection {
        stem: "type_missing_name",
        message: "types[0] has no \"name\"",
    },
    Rejection {
        stem: "name_not_a_string",
        message: "types[0].name should be a string",
    },
    Rejection {
        stem: "fields_not_an_array",
        message: "types[0].fields should be an array",
    },
    Rejection {
        stem: "field_missing_type",
        message: "types[0].fields[0] has no \"type\"",
    },
    Rejection {
        stem: "field_unexpected_key",
        message: "\"doc\"",
    },
    Rejection {
        stem: "unknown_kind",
        message: "kind \"u64\"",
    },
    Rejection {
        stem: "scalar_with_payload",
        message: "\"of\"",
    },
    Rejection {
        stem: "list_without_of",
        message: "has no \"of\"",
    },
    Rejection {
        stem: "named_without_name",
        message: "root has no \"name\"",
    },
    Rejection {
        stem: "kind_not_a_string",
        message: "root.kind should be a string",
    },
    Rejection {
        stem: "ty_not_an_object",
        message: "root should be an object",
    },
    Rejection {
        stem: "ty_too_deep",
        message: "type constructors deep",
    },
    Rejection {
        stem: "deeper_than_the_store",
        message: "past the store's limit",
    },
    Rejection {
        stem: "illegal_type_name",
        message: "\"2App\" is not a legal type name",
    },
    Rejection {
        stem: "illegal_key",
        message: "is not a legal path segment",
    },
    Rejection {
        stem: "duplicate_key",
        message: "two fields on the key \"zoom\"",
    },
    Rejection {
        stem: "name_collision",
        message: "two different types are both named \"App\"",
    },
    Rejection {
        stem: "cycle",
        message: "recursive type: Node -> Node",
    },
    Rejection {
        stem: "dangling_reference",
        message: "no type named \"Editor\" is defined",
    },
];

/// Valid schemas the emitters must refuse, and what they must say.
const UNEMITTABLE: &[Rejection] = &[
    Rejection {
        stem: "scalar_root",
        message: "needs a named struct at the root",
    },
    Rejection {
        stem: "optional_in_a_list",
        message: "puts an optional inside a container",
    },
    Rejection {
        stem: "nested_optional",
        message: "puts an optional inside a container",
    },
    Rejection {
        stem: "key_with_a_space",
        message: "ASCII letters, digits or underscores",
    },
    Rejection {
        stem: "non_ascii_key",
        message: "ASCII letters, digits or underscores",
    },
    Rejection {
        stem: "reserved_key",
        message: "does not name a Dart and TypeScript identifier",
    },
    Rejection {
        stem: "key_the_emitter_owns",
        message: "does not name a Dart and TypeScript identifier",
    },
    Rejection {
        stem: "key_starting_with_a_digit",
        message: "does not name a Dart and TypeScript identifier",
    },
    Rejection {
        stem: "accessor_collision",
        message: "both want the accessor \"fontSize\"",
    },
    Rejection {
        stem: "declaration_collision",
        message: "both want the name \"AppEditorClient\"",
    },
];

#[test]
fn every_malformed_schema_is_refused_with_a_message_naming_the_problem() {
    for rejection in REFUSED {
        let text = support::read(&format!("schema/negative/{}.json", rejection.stem));
        let error = read_schema(&text)
            .err()
            .unwrap_or_else(|| panic!("{} should have been refused", rejection.stem));
        assert!(
            error.to_string().contains(rejection.message),
            "{} should mention {:?}, said: {error}",
            rejection.stem,
            rejection.message
        );
    }
}

#[test]
fn every_unemittable_schema_reads_but_does_not_emit() {
    for rejection in UNEMITTABLE {
        let text = support::read(&format!("schema/unemittable/{}.json", rejection.stem));
        // It has to *read*, or the fixture is in the wrong directory and the
        // emitter's rejection was never reached.
        let schema = read_schema(&text)
            .unwrap_or_else(|e| panic!("{} should be a valid schema: {e}", rejection.stem));
        let error = generate(&schema, &Options::new("test", "test"))
            .err()
            .unwrap_or_else(|| panic!("{} should have been refused", rejection.stem));
        assert!(
            error.to_string().contains(rejection.message),
            "{} should mention {:?}, said: {error}",
            rejection.stem,
            rejection.message
        );
    }
}

#[test]
fn every_fixture_on_disk_is_claimed_by_an_expectation() {
    // A fixture nobody asserts on is a file that looks like a test and is not
    // one. Both directions are checked: an unclaimed file, and an expectation
    // whose file was deleted.
    for (directory, expected) in [
        (
            "negative",
            REFUSED.iter().map(|r| r.stem).collect::<BTreeSet<_>>(),
        ),
        (
            "unemittable",
            UNEMITTABLE.iter().map(|r| r.stem).collect::<BTreeSet<_>>(),
        ),
    ] {
        let found: BTreeSet<String> = support::schema_files(directory)
            .iter()
            .map(|path| support::stem(path))
            .collect();
        let expected: BTreeSet<String> = expected.into_iter().map(str::to_string).collect();
        assert_eq!(
            found, expected,
            "schema/{directory} and its expectations disagree"
        );
    }
}

#[test]
fn a_reader_rejection_and_an_emitter_rejection_are_different_types() {
    // The split is the point: one says "this file is not a schema", the other
    // says "this schema has no spelling in the target languages". Collapsing
    // them would leave a developer unable to tell a corrupt file from a type
    // definition that needs changing.
    let malformed = read_schema("{").expect_err("not JSON");
    assert!(matches!(malformed, ReadError::Json(_)));

    let scalar_root = read_schema(&support::read("schema/unemittable/scalar_root.json"))
        .expect("a scalar root is a valid schema");
    let refused = generate(&scalar_root, &Options::new("test", "test")).expect_err("not emittable");
    assert!(matches!(refused, EmitError::RootNotNamed { .. }));
}
