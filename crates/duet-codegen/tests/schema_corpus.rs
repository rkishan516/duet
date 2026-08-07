//! Generates and verifies `corpus/schema-corpus.json`.
//!
//! The three tests mirror `crates/duet-protocol/tests/wire_corpus.rs`, because
//! the failure they guard against is the same one: a committed artifact that
//! three languages conform to, quietly describing something the code no longer
//! does.
//!
//! - [`corpus_matches_the_committed_file`] runs in CI: regenerate in memory,
//!   compare bytes.
//! - [`rust_satisfies_its_own_corpus`] checks every claim in the **committed
//!   file** — the bytes the guests are handed, not a fresh generation — against
//!   an independently written validator. If Rust cannot satisfy what Rust
//!   produced, every guest is conforming to a phantom.
//! - `regenerate_corpus` is `#[ignore]`d and writes the file. Run by hand.
//!
//! Two more are coverage floors. The tests above check what is *there*, not what
//! is missing, and a corpus that lost half its entries would pass both.

mod support;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;

use duet_codegen::Plan;
use duet_core::{Path, Store, Value};
use duet_schema::{Schema, Ty, TypeDef};
use serde_json::Value as Json;
use support::{Fixture, admits, schema_corpus as corpus};

/// Reads the committed corpus, or says how to make one.
fn committed() -> String {
    let path = corpus::corpus_path();
    fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "cannot read {}: {e}\n\nRegenerate it with:\n    {}",
            path.display(),
            corpus::GENERATOR
        )
    })
}

/// Parses the committed corpus.
fn parsed() -> Json {
    serde_json::from_str(&committed())
        .unwrap_or_else(|e| panic!("the corpus is not valid JSON: {e}"))
}

/// A required string field of a corpus object.
fn text<'a>(object: &'a Json, field: &str) -> &'a str {
    object[field]
        .as_str()
        .unwrap_or_else(|| panic!("a corpus entry is missing a string {field:?}: {object}"))
}

/// A required array field of a corpus object.
fn array<'a>(object: &'a Json, field: &str) -> &'a [Json] {
    object[field]
        .as_array()
        .unwrap_or_else(|| panic!("a corpus entry is missing an array {field:?}: {object}"))
        .as_slice()
}

/// Decodes one of the corpus's wire strings back to a value.
fn value(wire: &str) -> Value {
    let json: Json =
        serde_json::from_str(wire).unwrap_or_else(|e| panic!("{wire} should be valid JSON: {e}"));
    duet_codec::decode_value(&json).unwrap_or_else(|e| panic!("{wire} should decode: {e}"))
}

/// The fixture, schema and plan behind one corpus schema entry.
fn resolve(entry: &Json) -> (&'static Fixture, Schema, Plan) {
    let source = text(entry, "source");
    let fixture = support::FIXTURES
        .iter()
        .find(|f| f.schema == source)
        .unwrap_or_else(|| panic!("the corpus names a schema no fixture provides: {source}"));
    let schema = support::schema(fixture.schema);
    let plan = Plan::build(&schema)
        .unwrap_or_else(|e| panic!("{} should be emittable: {e}", fixture.schema));
    (fixture, schema, plan)
}

/// Checks the accept and reject claims one entry makes about one type.
fn check_admission(what: &str, entry: &Json, ty: &Ty, optional: bool, types: &[TypeDef]) {
    assert_eq!(
        text(entry, "ty"),
        corpus::spell(ty),
        "{what}: the corpus spells its type wrongly"
    );
    assert_eq!(
        entry["optional"].as_bool(),
        Some(optional),
        "{what}: the corpus disagrees with the plan about optionality"
    );

    let accept = value(text(entry, "accept"));
    assert!(
        admits::admits(&accept, ty, optional, types),
        "{what}: the value the corpus admits is not one the type takes: {accept:?}"
    );

    let rejects = array(entry, "rejects");
    for reject in rejects {
        let refused = value(
            reject
                .as_str()
                .unwrap_or_else(|| panic!("{what}: a reject must be a wire string")),
        );
        assert!(
            !admits::admits(&refused, ty, optional, types),
            "{what}: the value the corpus rejects is one the type takes: {refused:?}"
        );
    }
    // `dynamic` is the only type with nothing to reject, and a corpus that
    // silently emitted an empty list elsewhere would turn every guest's reject
    // loop into a no-op for that field.
    assert_eq!(
        rejects.is_empty(),
        *ty == Ty::Dynamic,
        "{what}: only a dynamic type may have no rejects"
    );
}

#[test]
fn corpus_matches_the_committed_file() {
    let generated = corpus::render();
    let committed = committed();
    if committed != generated {
        panic!(
            "{} is out of date{}\n\nRegenerate it with:\n    {}",
            corpus::corpus_path().display(),
            support::first_difference(&committed, &generated),
            corpus::GENERATOR
        );
    }
}

#[test]
fn rust_satisfies_its_own_corpus() {
    let document = parsed();
    assert_eq!(document["version"].as_u64(), Some(corpus::VERSION));
    assert_eq!(
        text(&document, "generator"),
        corpus::GENERATOR,
        "the command that regenerates the file must stay accurate"
    );

    let schemas = array(&document, "schemas");
    assert_eq!(
        schemas.len(),
        support::FIXTURES.len(),
        "every fixture must appear exactly once"
    );

    for entry in schemas {
        let (fixture, schema, plan) = resolve(entry);
        assert_eq!(text(entry, "name"), fixture.stem);
        assert_eq!(text(entry, "root"), plan.root);
        check_seed(entry, &schema);
        check_types(entry, &plan, schema.types());
        check_paths(entry, &schema, &plan);
    }
}

/// The seed the corpus states is the seed the host starts from.
fn check_seed(entry: &Json, schema: &Schema) {
    assert_eq!(
        value(text(entry, "seed")),
        duet_schema::seed(schema),
        "the corpus states a seed the host does not start from; every live-host \
         assertion about an unwritten path would be against the wrong value"
    );
}

/// Every struct, its fields in declaration order, and what each admits.
fn check_types(entry: &Json, plan: &Plan, types: &[TypeDef]) {
    let stated = array(entry, "types");
    assert_eq!(
        stated.len(),
        plan.types.len(),
        "the corpus states a different number of types than the schema defines"
    );
    for (stated, planned) in stated.iter().zip(&plan.types) {
        assert_eq!(text(stated, "name"), planned.name);
        let fields = array(stated, "fields");
        assert_eq!(
            fields.len(),
            planned.fields.len(),
            "{}: field count",
            planned.name
        );
        // Declaration order, not sorted: it is what the generated constructors
        // use for positional arguments in both languages, so a corpus that
        // sorted it would let a reordering pass unnoticed.
        for (field, expected) in fields.iter().zip(&planned.fields) {
            assert_eq!(text(field, "key"), expected.key, "{}", planned.name);
            check_admission(
                &format!("{}.{}", planned.name, expected.key),
                field,
                &expected.ty.inner,
                expected.ty.optional,
                types,
            );
        }
    }
}

/// Every path the generated clients bind, with the seed and the admissions at it.
fn check_paths(entry: &Json, schema: &Schema, plan: &Plan) {
    let seed = duet_schema::seed(schema);
    let types = schema.types();
    let planned: BTreeMap<String, _> = corpus::paths(plan).into_iter().collect();

    let stated = array(entry, "paths");
    let named: BTreeSet<String> = stated.iter().map(|p| text(p, "path").to_string()).collect();
    assert_eq!(
        named,
        planned.keys().cloned().collect::<BTreeSet<_>>(),
        "the corpus and the emit plan disagree about which paths exist; a path \
         only the plan has is one no guest is checked against, and a path only \
         the corpus has is one no accessor binds"
    );

    for path_entry in stated {
        let path = text(path_entry, "path");
        let ty = &planned[path];
        check_admission(
            &format!("path {path:?}"),
            path_entry,
            &ty.inner,
            ty.optional,
            types,
        );

        // `null` means "no node here", which the wire spells differently from a
        // node holding `Value::Null` (`{"t":"n"}`). Keeping the two apart is
        // the whole `Option` story.
        let held = corpus::seeded_at(path, &seed);
        match (&path_entry["seed"], held) {
            (Json::Null, None) => {}
            (Json::String(wire), Some(actual)) => assert_eq!(
                value(wire),
                actual,
                "path {path:?}: the corpus states a seed the schema does not hold"
            ),
            (stated, actual) => {
                panic!("path {path:?}: the corpus states {stated} and the seed holds {actual:?}")
            }
        }
    }
}

#[test]
fn every_path_the_corpus_states_resolves_on_a_seeded_store() {
    // The corpus is the live-host run's input, and a path in it that addresses
    // nothing would make that run assert against a host that never had the
    // node. Checked against `duet_core::Store` — the store type the host runs —
    // rather than against the plan the paths came from.
    for fixture in support::FIXTURES {
        let schema = support::schema(fixture.schema);
        let plan = Plan::build(&schema)
            .unwrap_or_else(|e| panic!("{} should be emittable: {e}", fixture.schema));
        let store = Store::new(duet_schema::seed(&schema));

        for (path, ty) in corpus::paths(&plan) {
            let parsed = Path::parse(&path)
                .unwrap_or_else(|e| panic!("{}: {path:?} should parse: {e}", fixture.schema));
            let held = store.get(&parsed);
            // A path *below* a `None` struct genuinely addresses nothing, and
            // the corpus already says so by recording a null seed for it. Every
            // other path must resolve.
            if held.is_none() {
                assert!(
                    below_an_optional(&path, &plan),
                    "{}: {path:?} addresses nothing on a seeded store, and no \
                     ancestor of it is optional",
                    fixture.schema
                );
                continue;
            }
            // A required path seeded `Null` would read as a mismatch through
            // its own generated accessor before anything had been written —
            // except for `dynamic`, whose codec is the identity and for which
            // `Null` is a perfectly ordinary value.
            assert!(
                ty.optional || ty.inner == Ty::Dynamic || !matches!(held, Some(Value::Null)),
                "{}: {path:?} is required and seeded null",
                fixture.schema
            );
        }
    }
}

/// True if some ancestor of `path` is an optional struct, which is the only
/// legitimate reason a minted path addresses nothing on a seeded store.
fn below_an_optional(path: &str, plan: &Plan) -> bool {
    plan.classes
        .iter()
        .any(|class| class.optional && !class.path.is_empty() && path.starts_with(&class.path))
}

#[test]
fn the_corpus_covers_every_type_and_every_shape() {
    // The coverage floor. `tests/coverage.rs` already asserts the two schemas
    // between them reach every arm of the type language; this asserts the
    // *corpus* carries each arm out to a guest, which is a different claim — a
    // generator that skipped an arm would leave it uncovered in every language
    // at once, and nothing else here would notice.
    let document = parsed();
    let mut spellings: BTreeSet<String> = BTreeSet::new();
    let mut optional_seen = [false, false];
    let mut empty_rejects = false;
    let mut absent_seed = false;
    let mut named_path = false;

    for schema in array(&document, "schemas") {
        for entry in array(schema, "paths") {
            let spelling = text(entry, "ty");
            spellings.insert(spelling.to_string());
            let optional = entry["optional"].as_bool().expect("a path states optional");
            optional_seen[usize::from(optional)] = true;
            empty_rejects |= array(entry, "rejects").is_empty();
            absent_seed |= entry["seed"].is_null();
            named_path |= spelling.starts_with(char::is_uppercase);
        }
    }

    for arm in [
        Ty::Bool,
        Ty::Int,
        Ty::Float,
        Ty::Str,
        Ty::Bytes,
        Ty::Dynamic,
        Ty::Float.list(),
        Ty::Int.map(),
        Ty::Int.list().list(),
    ] {
        let want = corpus::spell(&arm);
        assert!(
            spellings.contains(&want),
            "no path in the corpus has type {want}; it is uncovered in every language"
        );
    }
    assert!(
        optional_seen[0] && optional_seen[1],
        "both polarities of `optional` must appear"
    );
    assert!(named_path, "a struct-typed path must appear");
    assert!(
        empty_rejects,
        "a `dynamic` path must appear, and it is the only one with no rejects — \
         without it a guest could skip the rejects check everywhere and pass"
    );
    assert!(
        absent_seed,
        "a path below a None struct must appear, seeded absent: it is the case \
         the three measured Option behaviours are about"
    );
    // And no spelling may be one this generator did not recognise.
    for spelling in &spellings {
        assert!(
            !spelling.starts_with("unspelled:"),
            "the corpus carries a Ty arm with no spelling: {spelling}"
        );
    }
}

#[test]
fn the_corpus_states_enough_to_drive_a_conformance_run() {
    // Pinned as literals, for the same reason `wire_corpus_test.dart` pins its
    // case counts: a corpus test that consumes whatever it is given is
    // worthless, because a file truncated to one entry would pass everything
    // above it. Changing these is a deliberate review step.
    const SCHEMAS: usize = 2;
    const PATHS: usize = 31;
    const TYPES: usize = 6;
    const FIELDS: usize = 29;

    let document = parsed();
    let schemas = array(&document, "schemas");
    assert_eq!(schemas.len(), SCHEMAS);

    let paths: usize = schemas.iter().map(|s| array(s, "paths").len()).sum();
    let types: usize = schemas.iter().map(|s| array(s, "types").len()).sum();
    let fields: usize = schemas
        .iter()
        .flat_map(|s| array(s, "types"))
        .map(|t| array(t, "fields").len())
        .sum();
    assert_eq!(paths, PATHS, "path count");
    assert_eq!(types, TYPES, "type count");
    assert_eq!(fields, FIELDS, "field count");
}

#[test]
#[ignore = "writes corpus/schema-corpus.json; run by hand, then review the diff"]
fn regenerate_corpus() {
    let path = corpus::corpus_path();
    let dir = path
        .parent()
        .unwrap_or_else(|| panic!("{} should have a parent", path.display()));
    fs::create_dir_all(dir).unwrap_or_else(|e| panic!("cannot create {}: {e}", dir.display()));
    fs::write(&path, corpus::render())
        .unwrap_or_else(|e| panic!("cannot write {}: {e}", path.display()));
}
