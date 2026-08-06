//! The validated contract: [`Schema`].

use std::collections::BTreeMap;

use duet_core::{MAX_VALUE_DEPTH, Path, Segment};

use crate::error::{SchemaError, SchemaErrors};
use crate::registry::Registry;
use crate::state::SharedState;
use crate::ty::{Ty, TypeDef, is_legal_key, is_legal_type_name};

/// The complete, **validated** description of one root type.
///
/// A `Schema` exists only if every check in [`Schema::of`] passed, so anything
/// holding one may assume: no cycles, no dangling references, every name a
/// legal identifier, every key a legal path segment that round-trips through
/// [`Path::parse`], and a declared nesting within
/// [`MAX_VALUE_DEPTH`].
///
/// # This, not the Rust types, is the contract
///
/// Increment 4's emitters read a rendered schema; increment 6's derive must
/// produce one byte-identical to a hand-written specification. Two independent
/// producers satisfying one artifact is what makes "one type definition, three
/// agreeing clients" a checkable claim rather than an assertion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schema {
    root: Ty,
    types: Vec<TypeDef>,
}

impl Schema {
    /// Derives and validates the schema of `T`.
    ///
    /// # Errors
    ///
    /// [`SchemaErrors`] carrying **every** distinct problem found, not just the
    /// first: a developer fixing type definitions benefits from seeing all of
    /// them at once. See [`SchemaError`] for the individual reasons.
    pub fn of<T: SharedState>() -> Result<Schema, SchemaErrors> {
        let mut registry = Registry::new();
        let root = T::schema(&mut registry);
        let (types, mut errors) = registry.into_parts();

        // A cyclic or dangling graph makes the rest meaningless — a depth walk
        // over a cycle would not terminate — so stop once the shape is wrong.
        let order = match resolve(&root, &types) {
            Ok(order) => order,
            Err(structural) => {
                errors.extend(structural);
                return Err(SchemaErrors(deduped(errors)));
            }
        };
        errors.extend(check_names_and_keys(&types));
        errors.extend(check_depth(&root, &types, &order));

        if errors.is_empty() {
            Ok(Schema { root, types })
        } else {
            Err(SchemaErrors(deduped(errors)))
        }
    }

    /// What a value of this schema looks like at the root.
    pub fn root(&self) -> &Ty {
        &self.root
    }

    /// Every struct the schema defines, ordered by name.
    pub fn types(&self) -> &[TypeDef] {
        &self.types
    }

    /// The deepest nesting a value of this schema declares.
    ///
    /// Comparable directly with [`MAX_VALUE_DEPTH`]
    /// and with [`Value::depth`](duet_core::Value::depth). Recomputed rather
    /// than cached: a `Schema` is built once at startup, the graph is small,
    /// and a cached field is one more thing that can disagree with the data it
    /// describes.
    pub fn depth(&self) -> usize {
        let order = resolve(&self.root, &self.types).unwrap_or_default();
        declared_depth(&self.root, &self.types, &order)
    }
}

/// Drops repeats while keeping first-seen order.
///
/// [`Registry::define`] and [`resolve`] both detect recursion — the first as a
/// schema is built, the second however it was built — so a recursive type is
/// found twice and should be reported once.
fn deduped(errors: Vec<SchemaError>) -> Vec<SchemaError> {
    let mut seen: Vec<SchemaError> = Vec::with_capacity(errors.len());
    for error in errors {
        if !seen.contains(&error) {
            seen.push(error);
        }
    }
    seen
}

/// Checks that every [`Ty::Named`] resolves and that the graph is acyclic,
/// returning the definitions in dependency order (each type after everything it
/// references).
///
/// # Iterative on purpose
///
/// This is the walk that has to survive a pathological schema, and a Rust stack
/// overflow is an abort rather than a catchable error. [`Registry::define`]
/// already refuses recursion as a schema is *built*; this refuses it however it
/// was built, including through a hand-written [`SharedState`] impl that
/// constructed a [`Ty::Named`] directly.
fn resolve(root: &Ty, types: &[TypeDef]) -> Result<Vec<String>, Vec<SchemaError>> {
    let by_name: BTreeMap<&str, &TypeDef> = types.iter().map(|t| (t.name.as_str(), t)).collect();
    let mut walker = Walker::default();
    // Definitions unreachable from the root are walked too: they are still part
    // of the schema, and silently skipping their checks would make a schema's
    // validity depend on which fields happen to reference what.
    let mut starts = root.referenced_names();
    starts.extend(types.iter().map(|t| t.name.as_str()));
    for start in starts {
        walker.walk(start, &by_name);
    }
    if walker.errors.is_empty() {
        Ok(walker.order)
    } else {
        Err(walker.errors)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Visit {
    Open,
    Done,
}

/// The state of one depth-first walk over the type graph.
#[derive(Default)]
struct Walker<'a> {
    state: BTreeMap<&'a str, Visit>,
    /// The chain of types currently open, so a cycle is reported as the path
    /// that closes it rather than just the name it closed on.
    chain: Vec<&'a str>,
    order: Vec<String>,
    errors: Vec<SchemaError>,
}

impl<'a> Walker<'a> {
    /// Visits `start` and everything it references, appending each type to
    /// `order` after all of its dependencies.
    fn walk(&mut self, start: &'a str, by_name: &BTreeMap<&'a str, &'a TypeDef>) {
        // `false` = descend into this node; `true` = its children are finished.
        let mut pending: Vec<(&'a str, bool)> = vec![(start, false)];
        while let Some((name, closing)) = pending.pop() {
            if closing {
                self.chain.pop();
                self.state.insert(name, Visit::Done);
                self.order.push(name.to_string());
                continue;
            }
            match self.state.get(name) {
                Some(Visit::Done) => continue,
                Some(Visit::Open) => {
                    self.errors.push(self.cycle_through(name));
                    continue;
                }
                None => {}
            }
            let Some(def) = by_name.get(name) else {
                self.errors.push(SchemaError::UnknownType {
                    name: name.to_string(),
                });
                self.state.insert(name, Visit::Done);
                continue;
            };
            self.state.insert(name, Visit::Open);
            self.chain.push(name);
            pending.push((name, true));
            for field in &def.fields {
                for child in field.ty.referenced_names() {
                    pending.push((child, false));
                }
            }
        }
    }

    /// The cycle closed by reaching `name` while it is still open: the open
    /// chain from its first appearance onwards, with `name` repeated at the end.
    fn cycle_through(&self, name: &str) -> SchemaError {
        let start = self
            .chain
            .iter()
            .position(|open| *open == name)
            .unwrap_or(0);
        let mut chain: Vec<String> = self.chain[start..].iter().map(|s| s.to_string()).collect();
        chain.push(name.to_string());
        SchemaError::Recursive { chain }
    }
}

/// Identifier legality, path-key grammar, and the [`Path::parse`] round trip.
fn check_names_and_keys(types: &[TypeDef]) -> Vec<SchemaError> {
    let mut errors = Vec::new();
    for def in types {
        if !is_legal_type_name(&def.name) {
            errors.push(SchemaError::IllegalName {
                name: def.name.clone(),
            });
        }
        for field in &def.fields {
            if !is_legal_key(&field.key) || !key_round_trips(&field.key) {
                errors.push(SchemaError::IllegalKey {
                    type_name: def.name.clone(),
                    key: field.key.clone(),
                });
            }
        }
    }
    errors
}

/// True if `key` survives being rendered into a path string and parsed back as
/// exactly one key segment holding exactly `key`.
///
/// This is the check [`Path::from_segments`](duet_core::Path::from_segments)
/// only makes as a `debug_assert!`, run here unconditionally and against the
/// real parser rather than re-derived from its grammar. Every path a generated
/// client mints is a string literal built from these keys, so a key that did not
/// round-trip would produce a literal addressing a *different* path — one
/// segment in, two out.
fn key_round_trips(key: &str) -> bool {
    matches!(
        Path::parse(key).as_ref().map(Path::segments),
        Ok([Segment::Key(parsed)]) if parsed == key
    )
}

/// Rejects a schema whose declared structure alone is deeper than the store
/// accepts.
///
/// Every write of such a root would be refused by
/// [`Store::set`](duet_core::Store::set), so it is a schema that can never be
/// installed — worth finding at startup rather than on the first write.
fn check_depth(root: &Ty, types: &[TypeDef], order: &[String]) -> Vec<SchemaError> {
    let depth = declared_depth(root, types, order);
    if depth > MAX_VALUE_DEPTH {
        vec![SchemaError::TooDeep {
            depth,
            max: MAX_VALUE_DEPTH,
        }]
    } else {
        Vec::new()
    }
}

/// The nesting the root's declared structure implies.
fn declared_depth(root: &Ty, types: &[TypeDef], order: &[String]) -> usize {
    let by_name: BTreeMap<&str, &TypeDef> = types.iter().map(|t| (t.name.as_str(), t)).collect();
    let mut table: BTreeMap<&str, usize> = BTreeMap::new();
    // `order` is dependency order, so every name a type references already has
    // an entry by the time that type is reached — no second pass, and no
    // recursion across type boundaries.
    for name in order {
        let Some(def) = by_name.get(name.as_str()) else {
            continue;
        };
        let deepest = def
            .fields
            .iter()
            .map(|field| ty_depth(&field.ty, &table))
            .max()
            .unwrap_or(0);
        // A struct is a `Value::Map`: one container, plus its deepest field.
        table.insert(def.name.as_str(), deepest + 1);
    }
    ty_depth(root, &table)
}

/// The nesting one [`Ty`] implies, resolving named types through `table`.
///
/// Iterative, for the same reason [`resolve`] is.
///
/// [`Ty::Dynamic`] counts as `0`. It has to: the depth of a [`Value`] a guest
/// put there is not a property of the schema, and assuming the worst would
/// reject every schema with a dynamic field anywhere. The store still bounds it
/// per write, at the moment it is written.
fn ty_depth(ty: &Ty, table: &BTreeMap<&str, usize>) -> usize {
    let mut deepest = 0usize;
    let mut pending = vec![(ty, 0usize)];
    while let Some((node, enclosing)) = pending.pop() {
        let here = match node {
            // `None` is a scalar and `Some(x)` is exactly `x`, so an option adds
            // no container of its own.
            Ty::Optional(inner) => {
                pending.push((inner, enclosing));
                continue;
            }
            Ty::List(inner) | Ty::Map(inner) => {
                pending.push((inner, enclosing + 1));
                enclosing + 1
            }
            Ty::Named(name) => enclosing + table.get(name.as_str()).copied().unwrap_or(0),
            _ => enclosing,
        };
        deepest = deepest.max(here);
    }
    deepest
}

#[cfg(test)]
#[path = "schema_tests.rs"]
mod tests;
