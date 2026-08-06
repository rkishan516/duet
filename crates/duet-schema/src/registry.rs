//! Collecting struct definitions while a schema is being derived.

use std::any::TypeId;
use std::collections::BTreeMap;

use crate::error::SchemaError;
use crate::ty::{FieldDef, Ty, TypeDef};

/// Accumulates the [`TypeDef`]s reachable from one root type.
///
/// [`SharedState::schema`](crate::SharedState::schema) is handed a `&mut
/// Registry` and returns the [`Ty`] describing itself. A struct registers its
/// definition through [`define`](Registry::define) and gets back a
/// [`Ty::Named`]; a primitive ignores the registry entirely.
///
/// # Why errors accumulate here instead of being returned
///
/// `schema` returns a bare `Ty`, not a `Result<Ty, _>`. That is deliberate: it
/// makes the derive's generated body a list of field descriptions with no
/// control flow in it — "generated code contains no logic a reviewer cannot
/// check by reading the diff" — and it means a hand-written impl cannot get
/// error propagation subtly wrong. Problems are recorded here and reported
/// once, by [`Schema::of`](crate::Schema::of), which is the only place that can
/// see the finished graph anyway.
#[derive(Debug, Default)]
pub struct Registry {
    /// Finished definitions, keyed by name. `BTreeMap` for a deterministic
    /// render order without a sort at the end.
    defined: BTreeMap<String, TypeDef>,
    /// The Rust type each name was claimed by, so a second, *different* type
    /// claiming it is a collision rather than a silent overwrite.
    origins: BTreeMap<String, TypeId>,
    /// The chain of [`define`](Registry::define) calls currently running. This
    /// is what turns a recursive type into an error instead of a stack
    /// overflow.
    in_progress: Vec<String>,
    errors: Vec<SchemaError>,
}

impl Registry {
    /// An empty registry.
    pub fn new() -> Registry {
        Registry::default()
    }

    /// Registers `T` under `name`, returning the [`Ty`] that refers to it.
    ///
    /// `fields` is run **at most once** per name: a type used in ten places is
    /// described once and referenced ten times. It is not run at all when the
    /// name is already defined, or when the call is recursive.
    ///
    /// # Recursion is an error, not a stack overflow
    ///
    /// A type that reaches itself — `struct Node { next: Option<Box<Node>> }` —
    /// would otherwise call `fields` from inside `fields`, forever, and a Rust
    /// stack overflow is an abort rather than a catchable error. When `name` is
    /// already on the in-progress chain this records
    /// [`SchemaError::Recursive`] carrying the whole chain and returns the
    /// reference *without* running `fields`, so the outer call unwinds
    /// normally and the caller gets a message naming the cycle.
    ///
    /// Recursive shared state is not merely unimplemented, it is unrepresentable:
    /// the store is a finite tree of [`Value`](duet_core::Value)s bounded at
    /// [`MAX_VALUE_DEPTH`](duet_core::MAX_VALUE_DEPTH), and a self-referential
    /// type has no finite set of paths for a generated client to address.
    ///
    /// # The `T` parameter
    ///
    /// It exists only to take `TypeId::of::<T>()`, which is what distinguishes
    /// "the same type described twice" (fine, and common — a type reached
    /// through two different fields) from "two different types that happen to
    /// share a name" (a collision: one would silently win, and every client
    /// would be generated against the wrong shape). Callers write
    /// `registry.define::<Self>("Editor", ...)`.
    pub fn define<T: 'static>(
        &mut self,
        name: &str,
        fields: impl FnOnce(&mut Registry) -> Vec<FieldDef>,
    ) -> Ty {
        if self.in_progress.iter().any(|open| open == name) {
            self.record_recursion(name);
            return Ty::Named(name.to_string());
        }
        if let Some(&origin) = self.origins.get(name) {
            if origin != TypeId::of::<T>() {
                self.errors.push(SchemaError::NameCollision {
                    name: name.to_string(),
                });
            }
            return Ty::Named(name.to_string());
        }
        self.origins.insert(name.to_string(), TypeId::of::<T>());

        self.in_progress.push(name.to_string());
        let described = fields(self);
        self.in_progress.pop();

        self.reject_duplicate_keys(name, &described);
        self.defined.insert(
            name.to_string(),
            TypeDef {
                name: name.to_string(),
                fields: described,
            },
        );
        Ty::Named(name.to_string())
    }

    /// Records the cycle that reaching `name` again closes, as the chain from
    /// its first appearance to here and back to it.
    fn record_recursion(&mut self, name: &str) {
        let start = self
            .in_progress
            .iter()
            .position(|open| open == name)
            .unwrap_or(0);
        let mut chain: Vec<String> = self.in_progress[start..].to_vec();
        chain.push(name.to_string());
        self.errors.push(SchemaError::Recursive { chain });
    }

    /// Two Rust fields renamed onto one wire key would occupy one map entry:
    /// one of them would be unreadable, and which one is not something the
    /// developer chose.
    fn reject_duplicate_keys(&mut self, name: &str, fields: &[FieldDef]) {
        let mut seen: Vec<&str> = Vec::with_capacity(fields.len());
        for field in fields {
            if seen.contains(&field.key.as_str()) {
                self.errors.push(SchemaError::DuplicateKey {
                    type_name: name.to_string(),
                    key: field.key.clone(),
                });
            }
            seen.push(&field.key);
        }
    }

    /// Every definition collected so far, ordered by name.
    pub(crate) fn into_parts(self) -> (Vec<TypeDef>, Vec<SchemaError>) {
        (self.defined.into_values().collect(), self.errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Alpha;
    struct Beta;

    fn leaf(registry: &mut Registry, name: &'static str) -> Ty {
        registry.define::<Alpha>(name, |_| vec![FieldDef::new("a", Ty::Int)])
    }

    #[test]
    fn a_definition_is_recorded_and_referenced_by_name() {
        let mut registry = Registry::new();
        assert_eq!(leaf(&mut registry, "Alpha"), Ty::Named("Alpha".to_string()));
        let (types, errors) = registry.into_parts();
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(types.len(), 1);
        assert_eq!(types[0].name, "Alpha");
        assert_eq!(types[0].fields, vec![FieldDef::new("a", Ty::Int)]);
    }

    #[test]
    fn the_body_runs_once_however_many_times_the_type_is_reached() {
        let mut registry = Registry::new();
        let mut runs = 0;
        for _ in 0..5 {
            registry.define::<Alpha>("Alpha", |_| {
                runs += 1;
                vec![FieldDef::new("a", Ty::Int)]
            });
        }
        assert_eq!(runs, 1, "a memoized definition must not re-run its body");
    }

    #[test]
    fn two_different_types_sharing_a_name_collide() {
        let mut registry = Registry::new();
        registry.define::<Alpha>("Config", |_| vec![FieldDef::new("a", Ty::Int)]);
        registry.define::<Beta>("Config", |_| vec![FieldDef::new("b", Ty::Str)]);
        let (_, errors) = registry.into_parts();
        assert_eq!(
            errors,
            vec![SchemaError::NameCollision {
                name: "Config".to_string()
            }]
        );
    }

    #[test]
    fn one_type_described_twice_is_not_a_collision() {
        let mut registry = Registry::new();
        registry.define::<Alpha>("Config", |_| vec![FieldDef::new("a", Ty::Int)]);
        registry.define::<Alpha>("Config", |_| vec![FieldDef::new("a", Ty::Int)]);
        let (types, errors) = registry.into_parts();
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(types.len(), 1);
    }

    #[test]
    fn a_self_referential_type_errors_instead_of_overflowing_the_stack() {
        let mut registry = Registry::new();
        registry.define::<Alpha>("Node", |inner| {
            let next = inner.define::<Alpha>("Node", |_| unreachable!("must not re-enter"));
            vec![FieldDef::new("next", next.optional())]
        });
        let (_, errors) = registry.into_parts();
        assert_eq!(
            errors,
            vec![SchemaError::Recursive {
                chain: vec!["Node".to_string(), "Node".to_string()],
            }]
        );
        assert_eq!(errors[0].to_string(), "recursive type: Node -> Node");
    }

    #[test]
    fn a_mutually_recursive_pair_reports_the_whole_chain() {
        let mut registry = Registry::new();
        registry.define::<Alpha>("A", |a| {
            let b = a.define::<Beta>("B", |b| {
                let back = b.define::<Alpha>("A", |_| unreachable!("must not re-enter"));
                vec![FieldDef::new("a", back)]
            });
            vec![FieldDef::new("b", b)]
        });
        let (_, errors) = registry.into_parts();
        assert_eq!(
            errors,
            vec![SchemaError::Recursive {
                chain: vec!["A".to_string(), "B".to_string(), "A".to_string()],
            }]
        );
        assert_eq!(errors[0].to_string(), "recursive type: A -> B -> A");
    }

    #[test]
    fn duplicate_wire_keys_in_one_type_are_rejected() {
        let mut registry = Registry::new();
        registry.define::<Alpha>("Dup", |_| {
            vec![
                FieldDef::new("zoom", Ty::Int),
                FieldDef::new("zoom", Ty::Str),
            ]
        });
        let (_, errors) = registry.into_parts();
        assert_eq!(
            errors,
            vec![SchemaError::DuplicateKey {
                type_name: "Dup".to_string(),
                key: "zoom".to_string(),
            }]
        );
    }

    #[test]
    fn nested_definitions_are_all_collected() {
        let mut registry = Registry::new();
        registry.define::<Alpha>("Outer", |outer| {
            let inner = outer.define::<Beta>("Inner", |_| vec![FieldDef::new("x", Ty::Float)]);
            vec![FieldDef::new("inner", inner)]
        });
        let (types, errors) = registry.into_parts();
        assert!(errors.is_empty(), "{errors:?}");
        // Ordered by name, not by definition order: the render must be stable
        // regardless of which field the walk happened to reach first.
        assert_eq!(
            types.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            ["Inner", "Outer"]
        );
    }
}
