//! Model and assembly for `corpus/schema-corpus.json`.
//!
//! One JSON file at the repository root, generated from Rust and consumed as a
//! peer by the Dart and JavaScript guest packages — the same arrangement, and
//! the same reasoning, as `corpus/wire-corpus.json`. It lives outside every
//! language's tree because none of them owns it.
//!
//! # What this corpus says, and what the wire corpus says
//!
//! The wire corpus is about the **envelope**: given this JSON text, what must a
//! decoder produce, and what must it refuse. It knows nothing about schemas.
//!
//! This corpus is about the **schema**: given a type definition, what wire keys
//! does each struct occupy, what paths does it mint, what does a store hold
//! before anything is written, and — for every one of those — which values the
//! type admits and which it must report as a mismatch.
//!
//! # The split with the live-host run
//!
//! Both halves exist because they fail for different reasons and cost different
//! amounts to run.
//!
//! **Here, with no process:** everything checkable against a guest's *own*
//! generated codecs. A guest can build the map a struct's fields describe and
//! ask its codec to decode it; it can swap one field for a foreign type and
//! require a refusal; it can walk the seed by path. All of that is arithmetic on
//! values, it runs inside `dart test` and `node --test` with no toolchain beyond
//! the guest's own, and it is the half that catches a codec bound to the wrong
//! type — an `int` field reading through a float codec.
//!
//! **There, with a host process:** everything that needs the boundary. Whether a
//! path *resolves* on a real store, whether a `set` at it is accepted or
//! refused and with which message, whether a subscription pushes, and what
//! happens below an `Option<Struct>` that is `None`. None of that is a property
//! of a value; all of it is a property of `duet-core`'s write rules, which is
//! precisely what a hand-transcribed fake host can get wrong.
//!
//! The live-host run *reads this file* for its inputs — the seed it expects,
//! the paths it must cover, the values to write — so the two halves cannot
//! disagree about what the schema says. They disagree only about what they can
//! observe.
//!
//! # Why `rejects` is a list and why it is sometimes empty
//!
//! A `list<float>` has two distinct ways to be wrong: it can be the wrong
//! container (a map), or the right container holding the wrong element. A codec
//! that ignored its element codec passes the first and fails the second, so one
//! reject value per field would be a coin flip about which bug is caught.
//!
//! `dynamic` is the one type with no rejects at all, because it admits every
//! value the wire can carry. Stating that explicitly — an empty list rather
//! than an omitted field — is what stops a guest quietly skipping the check for
//! every field, having found no `rejects` key anywhere.

use std::collections::BTreeMap;
use std::path::PathBuf;

use duet_codegen::Plan;
use duet_codegen::plan::PlannedTy;
use duet_core::{Path, Value};
use duet_schema::{Schema, Ty, TypeDef};
use serde_json::{Map as JsonMap, Value as Json};

use crate::support::{self, Fixture};

/// Schema version. Bump only for a change a guest reader must adapt to.
pub const VERSION: u64 = 1;

/// The exact command that regenerates the committed file.
pub const GENERATOR: &str =
    "cargo test -p duet-codegen --test schema_corpus -- --ignored regenerate_corpus";

/// Where the committed corpus lives: the repository root, not any one
/// language's tree, because all three implementations consume it as peers.
pub fn corpus_path() -> PathBuf {
    support::repo_root()
        .join("corpus")
        .join("schema-corpus.json")
}

/// A [`Value`] as the wire spells it.
fn wire(value: &Value) -> String {
    serde_json::to_string(&duet_codec::encode_value(value))
        .expect("a serde_json::Value always serializes")
}

/// A [`Ty`] as this corpus names it.
///
/// The scalar names are the schema document's own `kind` values, so a reader
/// holding `schema/app.json` and this file open sees one vocabulary. Containers
/// carry their element type in angle brackets, and a `named` type is spelled by
/// its name alone — unambiguous, because a schema type name must start with a
/// letter and be capitalised the way `duet-schema` requires, while every kind
/// name here is lowercase.
///
/// `optional` never appears: it is lifted out into the `optional` flag, exactly
/// as [`PlannedTy`] lifts it, because the two guest runtimes spell it as a
/// different *class* rather than as a different codec.
pub fn spell(ty: &Ty) -> String {
    match ty {
        Ty::Bool => "bool".to_string(),
        Ty::Int => "int".to_string(),
        Ty::Float => "float".to_string(),
        Ty::Str => "string".to_string(),
        Ty::Bytes => "bytes".to_string(),
        Ty::Dynamic => "dynamic".to_string(),
        Ty::Optional(inner) => format!("optional<{}>", spell(inner)),
        Ty::List(inner) => format!("list<{}>", spell(inner)),
        Ty::Map(inner) => format!("map<{}>", spell(inner)),
        Ty::Named(name) => name.clone(),
        // `Ty` is `#[non_exhaustive]`. A new arm with no spelling here would
        // otherwise be silently rendered as something a guest might match.
        other => format!("unspelled:{other:?}"),
    }
}

/// A value the type admits.
///
/// Fixed per type rather than varied per path, and that is deliberate. Path
/// identity is not established by a unique value — the live-host run establishes
/// it by writing through the typed accessor and reading back at the *wire*
/// path, which fails for a wrong path literal whatever value was written. A
/// value that varied per path would instead have to be recomputed identically
/// in three languages to be assertable at all.
pub fn admitted(ty: &Ty, types: &[TypeDef]) -> Value {
    match ty {
        Ty::Bool => Value::Bool(true),
        Ty::Int => Value::Int(7),
        Ty::Float => Value::Float(3.25),
        Ty::Str => Value::Str("sample".to_string()),
        Ty::Bytes => Value::Bytes(vec![1, 2, 3]),
        // Any value at all; a string is the one that cannot be confused with
        // the seed, which is `Null`.
        Ty::Dynamic => Value::Str("anything".to_string()),
        // The inner value rather than `Null`: `Null` is admitted too, and the
        // guests already have `optional` to tell them so. Filling the field is
        // what makes a struct built from these decodable in one piece.
        Ty::Optional(inner) => admitted(inner, types),
        Ty::List(inner) => Value::List(vec![admitted(inner, types)]),
        Ty::Map(inner) => Value::Map(BTreeMap::from([("k".to_string(), admitted(inner, types))])),
        Ty::Named(name) => admitted_struct(name, types),
        other => panic!("no admitted value for the unhandled Ty arm {other:?}"),
    }
}

/// A struct with every field filled.
fn admitted_struct(name: &str, types: &[TypeDef]) -> Value {
    let Some(def) = types.iter().find(|t| t.name == name) else {
        panic!("{name} does not resolve; Schema should have refused this");
    };
    Value::Map(
        def.fields
            .iter()
            .map(|f| (f.key.clone(), admitted(&f.ty, types)))
            .collect(),
    )
}

/// Values the type must refuse, most specific first.
///
/// `optional` is **not** handled here: a field's optionality decides whether
/// `Null` belongs in the list, and that is decided by [`rejected_at`], which can
/// see it.
fn rejected(ty: &Ty, types: &[TypeDef]) -> Vec<Value> {
    match ty {
        Ty::Bool => vec![Value::Int(1)],
        // `Float` and `Str` both: an `int` field reading through a float codec
        // is the exact bug this increment is about, and `Int` travels as a
        // decimal *string* on the wire, so a decoder that read the tag loosely
        // would take a `Str` for one.
        Ty::Int => vec![Value::Float(1.0), Value::Str("1".to_string())],
        Ty::Float => vec![Value::Int(1)],
        Ty::Str => vec![Value::Int(1)],
        // A list of small integers is what `Vec<u8>` lowers to, and base64 text
        // is what `Bytes` looks like on the wire. Both are the near misses.
        Ty::Bytes => vec![
            Value::List(vec![Value::Int(1)]),
            Value::Str("AQID".to_string()),
        ],
        // Nothing is a mismatch for `dynamic`. See this module's header.
        Ty::Dynamic => Vec::new(),
        Ty::Optional(inner) => rejected(inner, types),
        Ty::List(inner) => wrong_container(Value::Map(BTreeMap::new()), inner, types, Value::List),
        Ty::Map(inner) => wrong_container(Value::List(Vec::new()), inner, types, |items| {
            Value::Map(BTreeMap::from([("k".to_string(), items[0].clone())]))
        }),
        // A struct refuses anything that is not a map, and a map missing a
        // field it was promised.
        Ty::Named(_) => vec![Value::Int(1), Value::Map(BTreeMap::new())],
        other => panic!("no rejected values for the unhandled Ty arm {other:?}"),
    }
}

/// The two ways a container can be wrong: the wrong kind of container, and the
/// right kind holding an element of the wrong type.
fn wrong_container(
    wrong_kind: Value,
    inner: &Ty,
    types: &[TypeDef],
    rebuild: impl Fn(Vec<Value>) -> Value,
) -> Vec<Value> {
    let mut found = vec![wrong_kind];
    // A `list<dynamic>` has no wrong element, so this genuinely yields one
    // reject rather than two.
    if let Some(bad) = rejected(inner, types).into_iter().next() {
        found.push(rebuild(vec![bad]));
    }
    found
}

/// Every value a field or path of this type must refuse.
///
/// A **required** field additionally refuses `Null`: the schema promised a `T`,
/// and `Null` is not one. An **optional** field does not, because `Null` is
/// precisely how `Option::None` is spelled. That one line is the difference
/// between `DuetField` and `DuetOptionalField` in both guests, so a corpus that
/// blurred it would let either of them stand in for the other.
///
/// A required `dynamic` refuses nothing at all, `Null` included: it is the one
/// type whose codec is the identity.
pub fn rejected_at(ty: &PlannedTy, types: &[TypeDef]) -> Vec<Value> {
    let mut found = rejected(&ty.inner, types);
    if !ty.optional && ty.inner != Ty::Dynamic {
        found.push(Value::Null);
    }
    found
}

/// One struct field, as the corpus states it.
fn field_json(key: &str, ty: &PlannedTy, types: &[TypeDef]) -> Json {
    let mut m = JsonMap::new();
    m.insert("key".to_string(), Json::String(key.to_string()));
    m.insert("ty".to_string(), Json::String(spell(&ty.inner)));
    m.insert("optional".to_string(), Json::Bool(ty.optional));
    m.insert(
        "accept".to_string(),
        Json::String(wire(&admitted(&ty.inner, types))),
    );
    m.insert(
        "rejects".to_string(),
        Json::Array(
            rejected_at(ty, types)
                .iter()
                .map(|v| Json::String(wire(v)))
                .collect(),
        ),
    );
    Json::Object(m)
}

/// One path, as the corpus states it.
fn path_json(path: &str, ty: &PlannedTy, seed: &Value, types: &[TypeDef]) -> Json {
    let mut m = JsonMap::new();
    m.insert("path".to_string(), Json::String(path.to_string()));
    m.insert("ty".to_string(), Json::String(spell(&ty.inner)));
    m.insert("optional".to_string(), Json::Bool(ty.optional));
    m.insert(
        "seed".to_string(),
        // `null` means "no node here", which is a different statement from a
        // node holding `Value::Null` — the wire spells that `{"t":"n"}`. Every
        // path below an `Option<Struct>` that seeds `None` is the first kind.
        match seeded_at(path, seed) {
            Some(value) => Json::String(wire(&value)),
            None => Json::Null,
        },
    );
    m.insert(
        "accept".to_string(),
        Json::String(wire(&admitted(&ty.inner, types))),
    );
    m.insert(
        "rejects".to_string(),
        Json::Array(
            rejected_at(ty, types)
                .iter()
                .map(|v| Json::String(wire(v)))
                .collect(),
        ),
    );
    Json::Object(m)
}

/// The value the seed holds at `path`, or `None` if nothing is there.
pub fn seeded_at(path: &str, seed: &Value) -> Option<Value> {
    let parsed = Path::parse(path).unwrap_or_else(|e| panic!("{path} should be a legal path: {e}"));
    seed.get(&parsed).cloned()
}

/// Every path a generated client binds, with the type at it.
///
/// Taken from [`Plan`] — the same description both emitters read — rather than
/// re-walked here. A second walk would be a second opinion about which paths
/// exist, and the one that disagreed silently would be this one.
pub fn paths(plan: &Plan) -> Vec<(String, PlannedTy)> {
    let mut found: Vec<(String, PlannedTy)> = vec![(
        String::new(),
        PlannedTy {
            optional: false,
            inner: Ty::Named(plan.root.clone()),
        },
    )];
    for class in &plan.classes {
        for accessor in &class.accessors {
            found.push((accessor.path.clone(), accessor.ty.clone()));
        }
    }
    // A struct reached from two places would be planned twice; the paths are
    // still distinct, so this only removes the exact repeats a diamond makes.
    found.sort_by(|a, b| a.0.cmp(&b.0));
    found.dedup_by(|a, b| a.0 == b.0);
    found
}

/// One schema's whole entry.
fn schema_json(fixture: &Fixture, schema: &Schema) -> Json {
    let plan = Plan::build(schema)
        .unwrap_or_else(|e| panic!("{} should be emittable: {e}", fixture.schema));
    let seed = duet_schema::seed(schema);
    let types = schema.types();

    let type_entries: Vec<Json> = plan
        .types
        .iter()
        .map(|t| {
            let mut m = JsonMap::new();
            m.insert("name".to_string(), Json::String(t.name.clone()));
            m.insert(
                "fields".to_string(),
                Json::Array(
                    t.fields
                        .iter()
                        .map(|f| field_json(&f.key, &f.ty, types))
                        .collect(),
                ),
            );
            Json::Object(m)
        })
        .collect();

    let path_entries: Vec<Json> = paths(&plan)
        .iter()
        .map(|(path, ty)| path_json(path, ty, &seed, types))
        .collect();

    let mut m = JsonMap::new();
    m.insert("name".to_string(), Json::String(fixture.stem.to_string()));
    m.insert(
        "source".to_string(),
        Json::String(fixture.schema.to_string()),
    );
    m.insert("root".to_string(), Json::String(plan.root.clone()));
    m.insert("seed".to_string(), Json::String(wire(&seed)));
    m.insert("types".to_string(), Json::Array(type_entries));
    m.insert("paths".to_string(), Json::Array(path_entries));
    Json::Object(m)
}

/// The whole corpus document.
pub fn document() -> Json {
    let schemas: Vec<Json> = support::FIXTURES
        .iter()
        .map(|fixture| schema_json(fixture, &support::schema(fixture.schema)))
        .collect();
    let mut m = JsonMap::new();
    m.insert("version".to_string(), Json::Number(VERSION.into()));
    m.insert("generator".to_string(), Json::String(GENERATOR.to_string()));
    m.insert("schemas".to_string(), Json::Array(schemas));
    Json::Object(m)
}

/// The exact bytes the committed file must contain.
///
/// `serde_json::Map` is a `BTreeMap` in this workspace, so object keys come out
/// in one deterministic order and the snapshot is stable. Pretty-printed with a
/// trailing newline so the diff is reviewable and the file is a well-behaved
/// text file — the same rules `corpus/wire-corpus.json` follows.
pub fn render() -> String {
    let mut text =
        serde_json::to_string_pretty(&document()).expect("the corpus document always serializes");
    text.push('\n');
    text
}
