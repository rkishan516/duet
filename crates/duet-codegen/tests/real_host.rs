//! Every path literal in the committed goldens, resolved against a real store.
//!
//! # The check the goldens cannot make
//!
//! A golden test proves the emitter still emits what it emitted before. If the
//! very first run had minted `'editr.zoom'`, or `'editor.fontSize'` where the
//! host owns `editor.font_size`, the golden would have recorded it and every
//! run since would have agreed. Nothing in a byte comparison can notice that a
//! path addresses **nothing**.
//!
//! So this takes its input from the *artifact* rather than from the producer: it
//! scans the committed `.dart` and `.ts` files for path literals and resolves
//! each one against a `duet_core::Store` — the same store type the host runs.
//! A path that does not exist, exists but holds a different kind of node, or
//! cannot be written to, fails here.
//!
//! That makes the input reachable from a real failure in three separate ways:
//! misspell a key in an emitter and the path stops resolving; camel-case a path
//! segment and it stops resolving; map `int` to a float codec and the codec at
//! the path disagrees with its schema type. `tests/mutation.rs` performs
//! exactly those three perturbations and records which check notices.
//!
//! # What it still does not prove
//!
//! It resolves paths on the host's own store. It does not run a Dart or
//! JavaScript guest against a live host across a process boundary — that is
//! increment 5's `corpus/schema-corpus.json` plus the guest conformance runs.
//! What the two guest packages do today is drive their generated clients
//! against a fake host transcribed from `duet-core`'s write rules, which covers
//! the codecs and the wire text but not the boundary itself.

mod support;

use duet_core::{Path, Store, Value};
use support::checks::{self, Language};

#[test]
fn every_path_literal_in_the_goldens_addresses_a_real_node() {
    for fixture in support::FIXTURES {
        let schema = support::schema(fixture.schema);
        for (relative, _) in [
            (fixture.dart_path(), Language::Dart),
            (fixture.ts_path(), Language::TypeScript),
        ] {
            let text = support::read(&relative);
            let checked =
                checks::paths_resolve(&text, &schema).unwrap_or_else(|e| panic!("{relative}: {e}"));
            assert!(
                checked > 4,
                "{relative} yielded only {checked} bindings; the scanner is not finding them"
            );
        }
    }
}

#[test]
fn the_goldens_spell_every_wire_key_exactly_as_the_schema_does() {
    // Complements the resolution check, which would still pass if *both* the
    // schema and the emitter had been camel-cased together.
    for fixture in support::FIXTURES {
        let schema = support::schema(fixture.schema);
        for relative in [fixture.dart_path(), fixture.ts_path()] {
            let text = support::read(&relative);
            checks::segments_are_schema_keys(&text, &schema)
                .unwrap_or_else(|e| panic!("{relative}: {e}"));
        }
    }
}

#[test]
fn every_binding_uses_the_codec_its_schema_type_calls_for() {
    for fixture in support::FIXTURES {
        let schema = support::schema(fixture.schema);
        for (relative, language) in [
            (fixture.dart_path(), Language::Dart),
            (fixture.ts_path(), Language::TypeScript),
        ] {
            let text = support::read(&relative);
            checks::codecs_match_the_schema(&text, &schema, language)
                .unwrap_or_else(|e| panic!("{relative}: {e}"));
        }
    }
}

#[test]
fn a_camel_cased_path_would_not_resolve() {
    // The negative half: the check above only means something if a camel-cased
    // path genuinely fails against the host. Reaching for `editor.fontSize` on a
    // store that owns `editor.font_size` must find nothing — which is precisely
    // the silent failure the naming rule exists to prevent.
    let store = Store::new(Value::map([(
        "editor",
        Value::map([("font_size", Value::Int(12))]),
    )]));
    let real = Path::parse("editor.font_size").expect("a legal path");
    let camel = Path::parse("editor.fontSize").expect("a legal path");
    assert_eq!(store.get(&real), Some(&Value::Int(12)));
    assert_eq!(store.get(&camel), None);
}

#[test]
fn the_binding_scanner_finds_a_nested_codec_whole() {
    // The scanner has to balance parentheses, or a `duetListCodec<int>(…)`
    // binding would be read as ending at its inner `)` and every check that
    // compares a codec would compare a truncated one — and pass.
    let found = checks::bindings(
        "  DuetField<List<int>> get tags =>\n      \
         DuetField<List<int>>(router, 'tags', duetListCodec<int>(duetIntCodec));\n",
    );
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].path, "tags");
    assert_eq!(found[0].codec, "duetListCodec<int>(duetIntCodec)");
}

#[test]
fn the_binding_scanner_finds_the_root_binding_too() {
    // The root's own handle is bound at the empty path. A scanner that skipped
    // it would leave the one binding whose literal is not obviously a path
    // unchecked.
    let found = checks::bindings("DuetField<App>(router, '', const AppCodec());");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].path, "");
    assert_eq!(found[0].codec, "const AppCodec()");
}
