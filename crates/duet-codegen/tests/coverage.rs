//! The coverage floor: no arm of the schema's type language may exist without a
//! fixture that reaches it.
//!
//! # Why this is asserted rather than reviewed
//!
//! The other tests check what is *there*. None of them notices what is
//! missing — a `Ty` arm added in Phase 4b with no fixture would leave every
//! existing test green while the emitters had never once been run against it,
//! and the first person to use the new arm would be the one who found out.
//!
//! The chain that makes a new arm impossible to add quietly:
//!
//! 1. `duet-schema`'s `write_ty` matches `Ty` with **no `_` arm**, so a new
//!    variant fails to compile until it has a wire spelling.
//! 2. That spelling has to be added to [`duet_codegen::KINDS`], or the reader
//!    cannot read a schema using it.
//! 3. This test fails until a committed fixture contains it.
//! 4. `every_kind_can_be_emitted_in_every_position` fails until both emitters
//!    have a type and a codec for it.

mod support;

use std::collections::BTreeSet;

use duet_codegen::{KINDS, Options, generate, read_schema};

#[test]
fn every_kind_the_reader_accepts_appears_in_a_committed_fixture() {
    let mut found = BTreeSet::new();
    for fixture in support::FIXTURES {
        found.extend(kinds_in(&support::read(fixture.schema)));
    }
    let expected: BTreeSet<String> = KINDS.iter().map(|k| (*k).to_string()).collect();
    assert_eq!(
        found, expected,
        "the fixtures and the reader's accepted kinds disagree"
    );
}

#[test]
fn every_kind_can_be_emitted_in_every_position_it_is_legal_in() {
    // A kind can be in a fixture and still be unreachable in some position.
    // This drives each one through every position the emitters have to spell:
    // bare, inside a list, inside a map, and behind an option.
    for kind in KINDS {
        // `optional` is legal only as a field's own type — the emitters refuse
        // it inside a container, and `tests/negative.rs` pins that — so it is
        // exercised through the `optional` wrapper below rather than as a leaf.
        if *kind == "optional" {
            continue;
        }
        let leaf = leaf_of(kind);
        for (position, ty) in [
            ("bare", leaf.clone()),
            (
                "in a list",
                format!("{{\"kind\": \"list\", \"of\": {leaf}}}"),
            ),
            ("in a map", format!("{{\"kind\": \"map\", \"of\": {leaf}}}")),
            (
                "behind an option",
                format!("{{\"kind\": \"optional\", \"of\": {leaf}}}"),
            ),
        ] {
            let schema = read_schema(&document(&ty))
                .unwrap_or_else(|e| panic!("{kind} {position} should read: {e}"));
            let generated = generate(&schema, &Options::new("test", "test"))
                .unwrap_or_else(|e| panic!("{kind} {position} should emit: {e}"));
            assert!(
                generated.dart.contains("get field"),
                "{kind} {position} produced no Dart accessor"
            );
            assert!(
                generated.ts.contains("get field()"),
                "{kind} {position} produced no TypeScript accessor"
            );
        }
    }
}

#[test]
fn the_reader_lists_each_kind_once() {
    let unique: BTreeSet<&&str> = KINDS.iter().collect();
    assert_eq!(unique.len(), KINDS.len(), "KINDS repeats itself");
}

/// A minimal schema with one field of type `ty`, plus a `Leaf` to point at.
fn document(ty: &str) -> String {
    format!(
        "{{\"root\": {{\"kind\": \"named\", \"name\": \"Root\"}}, \"types\": [\
         {{\"fields\": [{{\"key\": \"zoom\", \"type\": {{\"kind\": \"float\"}}}}], \"name\": \"Leaf\"}}, \
         {{\"fields\": [{{\"key\": \"field\", \"type\": {ty}}}], \"name\": \"Root\"}}\
         ], \"version\": 1}}"
    )
}

/// One `Ty` node of the given kind.
fn leaf_of(kind: &str) -> String {
    match kind {
        "named" => "{\"kind\": \"named\", \"name\": \"Leaf\"}".to_string(),
        "list" | "map" | "optional" => {
            format!("{{\"kind\": \"{kind}\", \"of\": {{\"kind\": \"int\"}}}}")
        }
        scalar => format!("{{\"kind\": \"{scalar}\"}}"),
    }
}

/// Every `"kind": "…"` spelling in a schema document.
///
/// Read out of the raw text rather than out of a parsed `Ty`, so a fixture
/// naming a kind the reader silently ignored would still be counted — and
/// therefore still be compared against what the reader accepts.
fn kinds_in(text: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for after in text.split("\"kind\": \"").skip(1) {
        if let Some(end) = after.find('"') {
            found.insert(after[..end].to_string());
        }
    }
    found
}
