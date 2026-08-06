//! **The proof.** The derive, applied to Rust structs, produces the committed
//! `schema/app.json` and `schema/wide.json` byte for byte.
//!
//! # Why byte-identical, and against a file that predates the macro
//!
//! `schema/app.json` was hand-written in increment 3 and has been the input to
//! everything downstream ever since: `duet-codegen` generates the committed Dart
//! and TypeScript clients from it, `duet-host-stdio` embeds it with
//! `include_str!` to seed the host both guest conformance suites drive, and
//! `corpus/schema-corpus.json` restates every path in it. None of that was built
//! against this macro; all of it was built against that file.
//!
//! So the file is an **independent target**. Two producers — a human, then a
//! derive — satisfying one artifact is what makes "one type definition, three
//! agreeing clients" checkable rather than asserted. A test that regenerated the
//! specification through the derive and compared the derive against it would
//! pass for any macro at all, including one that spelled every key backwards.
//! `tests/mutation.rs` measures that this comparison can in fact fail.
//!
//! # The Rust below is the whole input
//!
//! No `FieldDef`, no `Registry::define`, no `Ty`. Three `#[derive(SharedState)]`
//! attributes and the field declarations, which is the claim the project has
//! been making since day one.

mod support;

use std::collections::{BTreeMap, HashMap};

use duet::{Bytes, Schema, SharedState, Value};

/// The application's root state — the type `schema/app.json` describes.
#[derive(Debug, Clone, PartialEq, SharedState)]
struct App {
    counter: i64,
    editor: Editor,
    title: String,
}

/// A nested struct, reached from `App` and from `Outer` alike.
///
/// One definition satisfying both committed schemas: `app.json` and `wide.json`
/// each describe an `Editor` with exactly these two fields, and if they ever
/// stopped agreeing, one of the two assertions below would fail rather than
/// both quietly passing against two different types.
#[derive(Debug, Clone, PartialEq, SharedState)]
struct Editor {
    zoom: f64,
    theme: String,
}

/// A struct reached through another struct, so `wide.json` covers a `named`
/// type at depth.
#[derive(Debug, Clone, PartialEq, SharedState)]
struct Outer {
    inner: Editor,
    depth: i64,
}

/// Every arm of the schema's type language, in one struct.
///
/// `crates/duet-codegen/tests/coverage.rs` asserts mechanically that `app` and
/// `wide` between them reach every [`Ty`](duet::Ty) arm. Deriving this one is
/// therefore the statement that the derive can express the **whole** accepted
/// surface, not just the corner of it `App` happens to use.
#[derive(Debug, Clone, PartialEq, SharedState)]
struct Wide {
    flag: bool,
    count: i64,
    ratio: f64,
    label: String,
    blob: Bytes,
    anything: Value,
    maybe_label: Option<String>,
    maybe_ratios: Option<Vec<f64>>,
    maybe_editor: Option<Editor>,
    tags: Vec<String>,
    matrix: Vec<Vec<i64>>,
    lookup: BTreeMap<String, i64>,
    editors: BTreeMap<String, Editor>,
    blobs: Vec<Bytes>,
    loose: HashMap<String, Value>,
    flags: Vec<bool>,
    outer: Outer,
    snake_case_field: String,
}

/// The rendered schema of `T`, or the reason it has none.
fn rendered<T: SharedState>() -> String {
    Schema::of::<T>()
        .unwrap_or_else(|e| panic!("the derived schema should be valid: {e}"))
        .render()
}

#[test]
fn the_derived_app_schema_is_byte_identical_to_the_committed_specification() {
    support::assert_matches_committed("schema/app.json", &rendered::<App>());
}

#[test]
fn the_derived_wide_schema_is_byte_identical_to_the_committed_specification() {
    support::assert_matches_committed("schema/wide.json", &rendered::<Wide>());
}

#[test]
fn the_host_both_conformance_suites_drive_is_seeded_from_these_bytes() {
    // Closes the loop rather than arguing it. `duet-host-stdio` embeds the two
    // schema files with `include_str!`, and it is that host the Dart and
    // TypeScript live-host suites talk to. Comparing the derive against the
    // text the host actually carries — not against the file, which is one more
    // hop — is what makes "the guests conform to the derived schema" a
    // measurement.
    let derived = [("app", rendered::<App>()), ("wide", rendered::<Wide>())];
    for (name, schema) in derived {
        let fixture = duet_host_stdio::FIXTURES
            .iter()
            .find(|f| f.name == name)
            .unwrap_or_else(|| panic!("the stdio host has no `{name}` fixture"));
        assert!(
            fixture.text == schema,
            "the host is seeded from {} , which is not what the derive produces",
            fixture.source
        );
    }
}

#[test]
fn the_derived_schema_registers_exactly_the_types_the_root_reaches() {
    // `Wide` and `Outer` are in this file too, and `App` must not drag them in:
    // a registry that collected every type in scope would produce a schema that
    // grew whenever an unrelated struct was added next to it.
    let app = Schema::of::<App>().expect("App has a schema");
    assert_eq!(
        app.types()
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>(),
        ["App", "Editor"]
    );

    let wide = Schema::of::<Wide>().expect("Wide has a schema");
    assert_eq!(
        wide.types()
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>(),
        ["Editor", "Outer", "Wide"]
    );
}
