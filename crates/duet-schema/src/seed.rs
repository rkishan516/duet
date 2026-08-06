//! The canonical starting value for a schema.

use duet_core::Value;

use crate::schema::Schema;
use crate::ty::{Ty, TypeDef};

/// The value a store must hold before anything has been written to it.
///
/// # Why a store needs one at all
///
/// [`duet_core::Value::set`] never creates intermediate nodes: writing
/// `editor.zoom` into an empty store fails rather than inventing `editor`. A
/// host that starts from [`Value::Null`] therefore refuses every write a
/// generated client makes, and a guest driven against it would be testing the
/// refusal path exclusively. The store has to *start* the right shape, and this
/// is the one description of that shape both a host and a corpus can share.
///
/// # What each arm seeds
///
/// | [`Ty`] | seed |
/// |---|---|
/// | `bool` | `Bool(false)` |
/// | `int` | `Int(0)` |
/// | `float` | `Float(0.0)` |
/// | `string` | `Str("")` |
/// | `bytes` | `Bytes([])` |
/// | `dynamic` | `Null` |
/// | `optional` | `Null` — Rust's `None` |
/// | `list` | `List([])` |
/// | `map` | `Map({})` |
/// | `named` | a `Map` holding every field's own seed |
///
/// An `optional` seeds to `Null` rather than to its inner type's seed, which is
/// what makes an `Option<Struct>` field *absent below itself*: `maybe_editor`
/// exists and holds `Null`, and `maybe_editor.zoom` addresses nothing. That is
/// the state the three measured `Option` behaviours are about — a child `get`
/// answers null, a child `set` fails, a `subscribe` succeeds — so seeding it any
/// other way would make that case unreachable.
///
/// A `list` and a `map` seed **empty**, so a schema with a `Vec<T>` cannot make
/// the seed's size a function of anything but the number of declared fields.
///
/// # Recursion is bounded by the schema, not by hope
///
/// This recurses only through [`Ty::Named`]: `list`, `map` and `optional` all
/// answer without looking inside themselves. So the recursion depth is the
/// schema's *struct* nesting, which [`Schema`] has already validated against
/// [`duet_core::MAX_VALUE_DEPTH`] — 61 — and which cannot be cyclic, because a
/// `Schema` exists only if the type graph is acyclic. A `Ty` nested a thousand
/// lists deep costs one frame here, not a thousand.
///
/// # A seeded store, and the write that needs it
///
/// ```
/// use duet_core::{Path, Value};
/// use duet_schema::{FieldDef, Schema, Ty, TypeDef, seed};
///
/// let schema = Schema::build(
///     Ty::Named("Editor".into()),
///     vec![TypeDef {
///         name: "Editor".into(),
///         fields: vec![
///             FieldDef::new("zoom", Ty::Float),
///             FieldDef::new("theme", Ty::Str),
///         ],
///     }],
/// )
/// .expect("a valid schema");
///
/// let mut root = seed(&schema);
/// assert_eq!(
///     root,
///     Value::map([("zoom", Value::Float(0.0)), ("theme", Value::Str(String::new()))]),
/// );
///
/// // And the point of it: a write at a nested path now lands.
/// let zoom = Path::parse("zoom").expect("a legal path");
/// assert!(root.set(&zoom, Value::Float(1.5)).is_ok());
/// ```
#[must_use]
pub fn seed(schema: &Schema) -> Value {
    seeded(schema.root(), schema.types())
}

/// The seed for one [`Ty`], resolving [`Ty::Named`] against `types`.
fn seeded(ty: &Ty, types: &[TypeDef]) -> Value {
    match ty {
        Ty::Bool => Value::Bool(false),
        Ty::Int => Value::Int(0),
        Ty::Float => Value::Float(0.0),
        Ty::Str => Value::Str(String::new()),
        Ty::Bytes => Value::Bytes(Vec::new()),
        Ty::List(_) => Value::List(Vec::new()),
        Ty::Map(_) => Value::Map(std::collections::BTreeMap::new()),
        Ty::Named(name) => seeded_struct(name, types),
        // `dynamic` and `optional` both mean "there may be nothing here", and
        // `Value::Null` is how this wire spells that. The `_` arm is required:
        // `Ty` is `#[non_exhaustive]`, and a Phase 4b arm with no seed of its
        // own is better served by `Null` — a value the store can hold and every
        // guest can report — than by a panic.
        _ => Value::Null,
    }
}

/// The seed for a named struct: every declared field, seeded.
///
/// A name that does not resolve seeds as [`Value::Null`]. [`Schema`] has already
/// rejected a dangling [`Ty::Named`], so this is unreachable through [`seed`];
/// answering rather than panicking is what keeps the function total for a
/// caller who assembled a [`TypeDef`] list by hand.
fn seeded_struct(name: &str, types: &[TypeDef]) -> Value {
    let Some(def) = types.iter().find(|t| t.name == name) else {
        return Value::Null;
    };
    Value::Map(
        def.fields
            .iter()
            .map(|field| (field.key.clone(), seeded(&field.ty, types)))
            .collect(),
    )
}

#[cfg(test)]
#[path = "seed_tests.rs"]
mod tests;
