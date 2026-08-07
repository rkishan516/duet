//! The validated contract: [`Schema`].

use std::collections::BTreeMap;

use duet_core::{MAX_VALUE_DEPTH, Path, Segment};

use crate::command::{CommandDef, is_legal_command_name};
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
/// `duet-codegen`'s emitters read a rendered schema, and
/// `#[derive(SharedState)]` produces one byte-identical to the hand-written
/// specification they were built against. Two independent producers satisfying
/// one artifact is what makes "one type definition, three agreeing clients" a
/// checkable claim rather than an assertion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schema {
    root: Ty,
    types: Vec<TypeDef>,
    commands: Vec<CommandDef>,
}

impl Schema {
    /// Derives and validates the schema of `T`, with no commands.
    ///
    /// # Errors
    ///
    /// [`SchemaErrors`] carrying **every** distinct problem found, not just the
    /// first: a developer fixing type definitions benefits from seeing all of
    /// them at once. See [`SchemaError`] for the individual reasons.
    pub fn of<T: SharedState>() -> Result<Schema, SchemaErrors> {
        Schema::of_with_commands::<T>(|_| Vec::new())
    }

    /// Derives and validates the schema of `T` together with a set of commands.
    ///
    /// # Why `describe` is a closure and not a `Vec`
    ///
    /// Commands are described into the **same [`Registry`]** the root is, and
    /// they have to be: a command taking or returning a struct must render as
    /// `{"kind": "named", …}` and that struct must appear in `types`, whether
    /// or not the root happens to reach it. A caller handed a finished
    /// `Vec<CommandDef>` would have had to build it against a registry of its
    /// own, and the definitions it registered would be in that one instead of
    /// this one — a schema whose commands name types it does not define.
    ///
    /// `duet_command::describe` is the implementation of this closure an
    /// embedder actually passes.
    ///
    /// # Errors
    ///
    /// [`SchemaErrors`] carrying every distinct problem found, including the
    /// command-specific ones: an illegal or repeated command name, an illegal
    /// or repeated parameter key, a parameter or return type naming a struct
    /// nothing defines, and a command whose types nest deeper than the store
    /// admits.
    pub fn of_with_commands<T: SharedState>(
        describe: impl FnOnce(&mut Registry) -> Vec<CommandDef>,
    ) -> Result<Schema, SchemaErrors> {
        let mut registry = Registry::new();
        let root = T::schema(&mut registry);
        let commands = describe(&mut registry);
        let (types, errors) = registry.into_parts();
        validate(root, types, commands, errors)
    }

    /// Builds and validates a schema from a root type and its definitions.
    ///
    /// The entry point for a schema that did **not** come from Rust types.
    /// `duet-codegen`'s reader parses a rendered schema back into a [`Ty`] and
    /// a `Vec<TypeDef>` and hands them here, so a parsed schema passes exactly
    /// the checks a derived one does rather than a reimplementation of them
    /// that could drift.
    ///
    /// `types` may arrive in any order and is sorted by name, because
    /// [`render`](Schema::render)'s byte-stability must not depend on how the
    /// caller happened to assemble the list.
    ///
    /// # Errors
    ///
    /// [`SchemaErrors`] carrying every distinct problem found. Beyond what
    /// [`Schema::of`] can produce, a hand-assembled list can also contain two
    /// [`TypeDef`]s sharing a name ([`SchemaError::NameCollision`]) or one
    /// [`TypeDef`] with two fields on one key ([`SchemaError::DuplicateKey`]),
    /// neither of which [`Registry`] can emit.
    pub fn build(root: Ty, types: Vec<TypeDef>) -> Result<Schema, SchemaErrors> {
        Schema::build_with_commands(root, types, Vec::new())
    }

    /// Builds and validates a schema from a root type, its definitions and its
    /// commands.
    ///
    /// What `duet-codegen`'s reader calls for a version-2 document. `commands`
    /// keeps the order the document gave it, which is the order it will be
    /// rendered back in.
    ///
    /// # Errors
    ///
    /// [`SchemaErrors`] carrying every distinct problem found — see
    /// [`Schema::build`] and [`Schema::of_with_commands`].
    pub fn build_with_commands(
        root: Ty,
        types: Vec<TypeDef>,
        commands: Vec<CommandDef>,
    ) -> Result<Schema, SchemaErrors> {
        validate(root, types, commands, Vec::new())
    }

    /// What a value of this schema looks like at the root.
    pub fn root(&self) -> &Ty {
        &self.root
    }

    /// Every struct the schema defines, ordered by name.
    pub fn types(&self) -> &[TypeDef] {
        &self.types
    }

    /// Every command the schema declares, ordered by name.
    ///
    /// Sorted, exactly as [`types`](Schema::types) is, and for the same reason:
    /// a command is reached by **name** and never by position, so the order it
    /// was registered in carries no meaning and letting it into the file would
    /// make reordering a `commands![…]` list a diff. A struct's *fields* are the
    /// one thing here that keeps declaration order, because generated
    /// constructors take them positionally.
    pub fn commands(&self) -> &[CommandDef] {
        &self.commands
    }

    /// The deepest nesting a value of this schema declares.
    ///
    /// Comparable directly with [`MAX_VALUE_DEPTH`]
    /// and with [`Value::depth`](duet_core::Value::depth). Recomputed rather
    /// than cached: a `Schema` is built once at startup, the graph is small,
    /// and a cached field is one more thing that can disagree with the data it
    /// describes.
    pub fn depth(&self) -> usize {
        let order = resolve(&self.root, &self.types, &self.commands).unwrap_or_default();
        let table = depth_table(&self.types, &order);
        deepest_declaration(&self.root, &self.commands, &table)
    }
}

/// The checks every schema passes, however it was assembled.
///
/// `errors` carries what the [`Registry`] already found, so a derived schema
/// reports registry problems and structural ones together.
fn validate(
    root: Ty,
    mut types: Vec<TypeDef>,
    mut commands: Vec<CommandDef>,
    mut errors: Vec<SchemaError>,
) -> Result<Schema, SchemaErrors> {
    // `Registry` yields types ordered by name already; a hand-assembled list
    // may not be, and `render`'s byte-stability must not depend on the order a
    // caller happened to build it in. Commands are sorted for the same reason
    // and by the same rule — see `Schema::commands`.
    types.sort_by(|a, b| a.name.cmp(&b.name));
    commands.sort_by(|a, b| a.name.cmp(&b.name));
    errors.extend(check_unique_names(&types));
    errors.extend(check_unique_command_names(&commands));

    // A cyclic or dangling graph makes the rest meaningless — a depth walk over
    // a cycle would not terminate — so stop once the shape is wrong.
    let order = match resolve(&root, &types, &commands) {
        Ok(order) => order,
        Err(structural) => {
            errors.extend(structural);
            return Err(SchemaErrors(deduped(errors)));
        }
    };
    errors.extend(check_names_and_keys(&types));
    errors.extend(check_duplicate_keys(&types));
    errors.extend(check_commands(&commands));
    errors.extend(check_depth(&root, &types, &commands, &order));

    if errors.is_empty() {
        Ok(Schema {
            root,
            types,
            commands,
        })
    } else {
        Err(SchemaErrors(deduped(errors)))
    }
}

/// Rejects two [`TypeDef`]s sharing a name.
///
/// [`Registry`] cannot produce this — it is keyed by name — but a
/// hand-assembled or parsed list can, and the by-name map [`resolve`] builds
/// would silently keep only one of them.
fn check_unique_names(types: &[TypeDef]) -> Vec<SchemaError> {
    // `types` is sorted by name, so duplicates are adjacent.
    types
        .windows(2)
        .filter(|pair| pair[0].name == pair[1].name)
        .map(|pair| SchemaError::NameCollision {
            name: pair[0].name.clone(),
        })
        .collect()
}

/// Rejects two commands sharing a name.
///
/// A guest names a command and nothing else, so two definitions on one name are
/// two things one `invoke` could mean — and which of them a registry would
/// actually run is decided by insertion order, not by the developer.
fn check_unique_command_names(commands: &[CommandDef]) -> Vec<SchemaError> {
    // `commands` is sorted by name, so duplicates are adjacent.
    commands
        .windows(2)
        .filter(|pair| pair[0].name == pair[1].name)
        .map(|pair| SchemaError::CommandCollision {
            name: pair[0].name.clone(),
        })
        .collect()
}

/// Rejects two fields of one type on one wire key.
///
/// [`Registry::define`] already refuses this as a schema is *built*; this
/// refuses it however it was built, including from a parsed document.
fn check_duplicate_keys(types: &[TypeDef]) -> Vec<SchemaError> {
    let mut errors = Vec::new();
    for def in types {
        let mut seen: Vec<&str> = Vec::with_capacity(def.fields.len());
        for field in &def.fields {
            if seen.contains(&field.key.as_str()) {
                errors.push(SchemaError::DuplicateKey {
                    type_name: def.name.clone(),
                    key: field.key.clone(),
                });
            }
            seen.push(&field.key);
        }
    }
    errors
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
fn resolve(
    root: &Ty,
    types: &[TypeDef],
    commands: &[CommandDef],
) -> Result<Vec<String>, Vec<SchemaError>> {
    let by_name: BTreeMap<&str, &TypeDef> = types.iter().map(|t| (t.name.as_str(), t)).collect();
    let mut walker = Walker::default();
    // Definitions unreachable from the root are walked too: they are still part
    // of the schema, and silently skipping their checks would make a schema's
    // validity depend on which fields happen to reference what.
    let mut starts = root.referenced_names();
    starts.extend(types.iter().map(|t| t.name.as_str()));
    // A command's parameter, return and error types are references into the
    // same graph. A command is very often the *only* thing that reaches an
    // error type — nothing in the state tree mentions it — so leaving these out
    // would let a schema name a struct it never defines, and the first sign of
    // it would be a generated client with a dangling type.
    starts.extend(commands.iter().flat_map(CommandDef::referenced_names));
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
            if !legal_wire_key(&field.key) {
                errors.push(SchemaError::IllegalKey {
                    type_name: def.name.clone(),
                    key: field.key.clone(),
                });
            }
        }
    }
    errors
}

/// Name legality and parameter keys, for every command.
///
/// A parameter key goes through [`legal_wire_key`] — the same predicate a
/// struct field's key does. The schema has **one** notion of a wire key, and
/// two would be one too many: a parameter occupies a key in the `args` map
/// exactly as a field occupies one in a `Value::Map`, and it has to survive as
/// an identifier in Dart and TypeScript exactly as a field's does.
fn check_commands(commands: &[CommandDef]) -> Vec<SchemaError> {
    let mut errors = Vec::new();
    for command in commands {
        if !is_legal_command_name(&command.name) {
            errors.push(SchemaError::IllegalCommandName {
                name: command.name.clone(),
            });
        }
        errors.extend(check_param_keys(command));
    }
    errors
}

/// One command's parameter keys: each legal, and no two the same.
fn check_param_keys(command: &CommandDef) -> Vec<SchemaError> {
    let mut errors = Vec::new();
    let mut seen: Vec<&str> = Vec::with_capacity(command.params.len());
    for param in &command.params {
        if !legal_wire_key(&param.key) {
            errors.push(SchemaError::IllegalParam {
                command: command.name.clone(),
                key: param.key.clone(),
            });
        }
        if seen.contains(&param.key.as_str()) {
            // Two parameters on one key occupy one entry of the `args` map: one
            // of the two can never be supplied, and which one is not something
            // the developer chose.
            errors.push(SchemaError::DuplicateParam {
                command: command.name.clone(),
                key: param.key.clone(),
            });
        }
        seen.push(&param.key);
    }
    errors
}

/// True if `key` is legal wherever the format admits a wire key.
fn legal_wire_key(key: &str) -> bool {
    is_legal_key(key) && key_round_trips(key)
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
fn check_depth(
    root: &Ty,
    types: &[TypeDef],
    commands: &[CommandDef],
    order: &[String],
) -> Vec<SchemaError> {
    let table = depth_table(types, order);
    let depth = deepest_declaration(root, commands, &table);
    if depth > MAX_VALUE_DEPTH {
        vec![SchemaError::TooDeep {
            depth,
            max: MAX_VALUE_DEPTH,
        }]
    } else {
        Vec::new()
    }
}

/// The deepest value this schema declares anywhere: the root, or a command's
/// arguments, return or error.
///
/// # Why a command's types count
///
/// A command's argument map and its reply travel the same wire the store's
/// values do, and `duet_protocol::dispatch_with` refuses a return past
/// [`MAX_VALUE_DEPTH`] at the moment it is produced. A schema declaring a
/// command that can only ever be refused is as broken as a root that can never
/// be installed, and it is worth finding at startup rather than on the first
/// call — which may be the first call in production, since nothing writes a
/// command's return into the store on the way there.
fn deepest_declaration(root: &Ty, commands: &[CommandDef], table: &BTreeMap<&str, usize>) -> usize {
    commands
        .iter()
        .flat_map(CommandDef::declared_types)
        .map(|ty| ty_depth(ty, table))
        .fold(ty_depth(root, table), usize::max)
}

/// How deep each named type nests, keyed by name.
fn depth_table<'a>(types: &'a [TypeDef], order: &[String]) -> BTreeMap<&'a str, usize> {
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
    table
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
