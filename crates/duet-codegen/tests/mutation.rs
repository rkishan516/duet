//! Mutation testing for the emitters: break one decision at a time and record
//! which check notices.
//!
//! # Why this file exists
//!
//! A check nobody has ever seen fail is a check nobody knows works. Every test
//! in this crate passes today; that is exactly the state a test whose input can
//! never reach the failure is also in. This file makes each check fail on
//! purpose, and asserts *which* one does — so the table in the report is
//! measured rather than reasoned about.
//!
//! The mutations are applied to generated **text**, standing in for an emitter
//! that made the wrong decision. Three kinds, one per thing the task named:
//!
//! | Mutation | Stands for |
//! |---|---|
//! | a path literal misspelled | an emitter that mints the wrong path |
//! | a path literal camel-cased | an emitter that applies the accessor rule to paths |
//! | a codec swapped for another | an emitter with a wrong type mapping |
//!
//! # What is measured, and what it means
//!
//! `goldens` is the byte comparison. It catches **every** mutation, and that is
//! the point about golden tests: catching everything is not the same as knowing
//! anything. It fires identically whether the change was a bug or a deliberate
//! improvement, so on its own it can only ever say "something moved" — and the
//! reviewer's response to it is to regenerate, which makes the mutation the new
//! truth. The other three checks are the ones that say *what is wrong*.

mod support;

use support::checks::{self, Language};

/// One deliberate corruption, and which checks must notice it.
struct Mutation {
    what: &'static str,
    from: &'static str,
    to: &'static str,
    /// Every check that must fail. Asserted exactly: a check that catches more
    /// than it should is as much a defect as one that catches less, because it
    /// stops localising the fault.
    caught_by: &'static [&'static str],
}

/// The corruptions, against `packages/duet/test/generated/wide.duet.dart`.
const MUTATIONS: &[Mutation] = &[
    Mutation {
        what: "a path literal misspelled",
        from: "router, 'outer.inner.zoom'",
        to: "router, 'outer.iner.zoom'",
        // All three: the segment check because `iner` is no schema key, the
        // resolution check because nothing is there, and the codec check
        // because it has to resolve the path before it can know what codec that
        // path calls for. Recorded as measured rather than as reasoned — the
        // codec check subsuming path resolution was a surprise, and pretending
        // otherwise would make this table fiction.
        caught_by: &[
            "goldens",
            "paths_resolve",
            "segments_are_schema_keys",
            "codecs_match_the_schema",
        ],
    },
    Mutation {
        what: "a path literal camel-cased, exactly as an accessor is",
        from: "router, 'snake_case_field'",
        to: "router, 'snakeCaseField'",
        caught_by: &[
            "goldens",
            "paths_resolve",
            "segments_are_schema_keys",
            "codecs_match_the_schema",
        ],
    },
    Mutation {
        what: "an int field bound to the float codec",
        from: "'count', duetIntCodec",
        to: "'count', duetFloatCodec",
        // Not a path problem, so neither path check can see it. The codec check
        // is the only one here that can — and it is a *restatement* of the
        // emitter's table, so the independent evidence is in the guest
        // packages' own tests, where a float codec on an `int` path reports a
        // mismatch against a host serving a `Value::Int`.
        caught_by: &["goldens", "codecs_match_the_schema"],
    },
    Mutation {
        what: "a struct field bound to the wrong struct's codec",
        from: "'outer.inner', const EditorCodec()",
        to: "'outer.inner', const OuterCodec()",
        caught_by: &["goldens", "codecs_match_the_schema"],
    },
    Mutation {
        what: "a required path pointed at a sibling of the wrong type",
        from: "router, 'ratio', duetFloatCodec",
        to: "router, 'count', duetFloatCodec",
        // The path still resolves — `count` exists — but it holds an `Int`
        // where a float codec was bound. Only the codec check can see this one,
        // and it is the case that says the two path checks are not enough on
        // their own.
        caught_by: &["goldens", "codecs_match_the_schema"],
    },
];

#[test]
fn every_mutation_is_caught_by_exactly_the_checks_that_should_catch_it() {
    let fixture = support::FIXTURES
        .iter()
        .find(|f| f.stem == "wide")
        .expect("the wide fixture");
    let schema = support::schema(fixture.schema);
    let original = support::read(&fixture.dart_path());

    for mutation in MUTATIONS {
        assert!(
            original.contains(mutation.from),
            "{}: {:?} is not in the golden, so the mutation changes nothing",
            mutation.what,
            mutation.from
        );
        let mutated = original.replacen(mutation.from, mutation.to, 1);
        assert_ne!(mutated, original, "{}: nothing changed", mutation.what);

        let verdicts = [
            ("goldens", mutated == original),
            (
                "paths_resolve",
                checks::paths_resolve(&mutated, &schema).is_ok(),
            ),
            (
                "segments_are_schema_keys",
                checks::segments_are_schema_keys(&mutated, &schema).is_ok(),
            ),
            (
                "codecs_match_the_schema",
                checks::codecs_match_the_schema(&mutated, &schema, Language::Dart).is_ok(),
            ),
        ];
        let caught: Vec<&str> = verdicts
            .iter()
            .filter(|(_, passed)| !passed)
            .map(|(name, _)| *name)
            .collect();
        assert_eq!(
            caught, mutation.caught_by,
            "{}: the checks that fired are not the ones that should have",
            mutation.what
        );
    }
}

#[test]
fn the_checks_all_pass_on_the_unmutated_file() {
    // Without this, a check that failed on *everything* would look like a
    // check that caught every mutation.
    let fixture = support::FIXTURES
        .iter()
        .find(|f| f.stem == "wide")
        .expect("the wide fixture");
    let schema = support::schema(fixture.schema);
    let original = support::read(&fixture.dart_path());

    checks::paths_resolve(&original, &schema).expect("the real golden resolves");
    checks::segments_are_schema_keys(&original, &schema).expect("the real golden uses wire keys");
    checks::codecs_match_the_schema(&original, &schema, Language::Dart)
        .expect("the real golden binds the right codecs");
}

#[test]
fn mutating_the_typescript_golden_is_caught_the_same_way() {
    // The two emitters are separate code. A check that only ever ran against
    // Dart would leave the TypeScript one covered by its golden alone.
    let fixture = support::FIXTURES
        .iter()
        .find(|f| f.stem == "wide")
        .expect("the wide fixture");
    let schema = support::schema(fixture.schema);
    let original = support::read(&fixture.ts_path());

    for (what, from, to) in [
        (
            "a camel-cased path",
            "router, 'snake_case_field'",
            "router, 'snakeCaseField'",
        ),
        (
            "a misspelled path",
            "router, 'outer.inner.zoom'",
            "router, 'outer.inner.zom'",
        ),
    ] {
        assert!(
            original.contains(from),
            "{what}: {from:?} is not in the golden"
        );
        let mutated = original.replacen(from, to, 1);
        assert!(
            checks::paths_resolve(&mutated, &schema).is_err(),
            "{what} was not caught in TypeScript"
        );
    }

    // `this.router, ` is part of the pattern because `['count', duetIntCodec…`
    // appears earlier, in the encoder's map literal: a mutation aimed at the
    // binding would otherwise land on the encoder and be invisible to a check
    // that only reads bindings.
    let mutated = original.replacen(
        "this.router, 'count', duetIntCodec",
        "this.router, 'count', duetFloatCodec",
        1,
    );
    assert_ne!(mutated, original, "the codec mutation changed nothing");
    assert!(
        checks::codecs_match_the_schema(&mutated, &schema, Language::TypeScript).is_err(),
        "a wrong codec was not caught in TypeScript"
    );
}
