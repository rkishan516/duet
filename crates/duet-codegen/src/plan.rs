//! The language-neutral emit plan: every declaration and every path literal,
//! decided once.
//!
//! Both emitters read this and neither decides anything. That is what keeps the
//! Dart client and the TypeScript client in step: a naming rule, a path or a
//! collision check that lived in one emitter could differ in the other, and the
//! difference would only show up as two guests addressing different nodes.
//!
//! Everything that can be refused is refused **here**, before a byte of source
//! exists, so a rejection names the schema field rather than appearing as a
//! Dart analyzer error in a generated file.

use duet_core::{Path, Segment};
use duet_schema::{Schema, Ty, TypeDef};

use crate::command::{PlannedCommand, plan_commands};
use crate::error::EmitError;
use crate::name::{is_emittable_key, is_usable_identifier, lower_camel, upper_camel};

/// The most struct-typed paths this emitter will expand into accessor classes.
///
/// Every struct-typed path gets a class of its own so that `client.editor.zoom`
/// can be a literal rather than a runtime join. A schema shaped like a diamond
/// — one type reached through several fields, several levels deep — expands
/// combinatorially, and a valid schema must not be able to turn into an
/// unbounded amount of generated source.
pub const MAX_CLASSES: usize = 256;

/// A field's type, with its optionality lifted out.
///
/// The two are separate because the guest runtime separates them: `inner` picks
/// the codec, and `optional` picks `DuetOptionalField` over `DuetField` and a
/// nullable data-class member over a plain one. `inner` is guaranteed to hold
/// no `optional` anywhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedTy {
    /// True when the field's own type is `optional`.
    pub optional: bool,
    /// What is inside the option, or the whole type when there is none.
    pub inner: Ty,
}

/// One field of one schema type, named for both target languages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedField {
    /// The wire key, **exactly** as the schema spells it.
    pub key: String,
    /// The accessor name: `lowerCamelCase`, idiomatic in Dart and TypeScript
    /// alike. Never appears in a path.
    pub accessor: String,
    /// What the field holds.
    pub ty: PlannedTy,
}

/// One schema struct: a data class and a codec in each language.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedType {
    /// The schema's own name for it.
    pub name: String,
    /// Its fields, in declaration order.
    pub fields: Vec<PlannedField>,
}

/// One field as reached from one particular place in the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedAccessor {
    /// The wire key.
    pub key: String,
    /// The accessor name.
    pub accessor: String,
    /// The **whole** wire path, from the store's root, as a validated literal.
    pub path: String,
    /// What the field holds.
    pub ty: PlannedTy,
    /// The accessor class for this field's own fields, when it is a struct.
    pub nested: Option<String>,
}

/// One generated accessor class: one struct type at one path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedClass {
    /// The generated class's name.
    pub name: String,
    /// The schema type it exposes.
    pub type_name: String,
    /// The wire path of the struct itself; `""` at the root.
    pub path: String,
    /// True when the field that reached this struct was itself optional.
    ///
    /// It changes what the class's own handle on the struct is: an
    /// `Option<Editor>` is a `DuetOptionalField<Editor>`, and reading one that
    /// is `None` must answer *none* rather than a mismatch. The root is never
    /// optional.
    pub optional: bool,
    /// One entry per field of `type_name`.
    pub accessors: Vec<PlannedAccessor>,
}

/// Everything both emitters need, and nothing either of them decides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// The schema's root type name; the generated client is named after it.
    pub root: String,
    /// Every struct, ordered by name as [`Schema::types`] orders them.
    pub types: Vec<PlannedType>,
    /// Every accessor class, root first, then breadth-first by path.
    pub classes: Vec<PlannedClass>,
    /// Every command, in the order [`Schema::commands`] gives them — sorted by
    /// name, which is what makes the generated method order a function of the
    /// schema rather than of how a registry was assembled.
    ///
    /// Empty for a schema that declares none, and the emitters write **no**
    /// commands class at all in that case, so a command-free schema produces
    /// byte-for-byte what it produced before commands existed.
    ///
    /// [`Schema::commands`]: duet_schema::Schema::commands
    pub commands: Vec<PlannedCommand>,
}

impl Plan {
    /// Plans the emission of `schema`.
    ///
    /// # Errors
    ///
    /// [`EmitError`] when the schema is valid but has no faithful spelling in
    /// Dart and TypeScript: a non-struct root, an option inside a container, a
    /// key that is not an identifier, a name collision, or more struct-typed
    /// paths than [`MAX_CLASSES`].
    pub fn build(schema: &Schema) -> Result<Plan, EmitError> {
        let Ty::Named(root) = schema.root() else {
            return Err(EmitError::RootNotNamed {
                found: format!("{:?}", schema.root()),
            });
        };
        let types = schema
            .types()
            .iter()
            .map(plan_type)
            .collect::<Result<Vec<_>, _>>()?;
        let classes = plan_classes(root, &types)?;
        let plan = Plan {
            root: root.clone(),
            types,
            classes,
            commands: plan_commands(schema.commands())?,
        };
        check_declaration_names(&plan)?;
        Ok(plan)
    }
}

/// Names one struct's fields and checks each one can be spelled.
fn plan_type(def: &TypeDef) -> Result<PlannedType, EmitError> {
    let mut fields: Vec<PlannedField> = Vec::with_capacity(def.fields.len());
    for field in &def.fields {
        if !is_emittable_key(&field.key) {
            return Err(EmitError::UnspellableKey {
                type_name: def.name.clone(),
                key: field.key.clone(),
                because: "a generated key must be ASCII letters, digits or underscores",
            });
        }
        let accessor = lower_camel(&field.key);
        if !is_usable_identifier(&accessor) {
            return Err(EmitError::UnspellableKey {
                type_name: def.name.clone(),
                key: field.key.clone(),
                because: "it does not name a Dart and TypeScript identifier the \
                          emitter has not already claimed",
            });
        }
        if fields.iter().any(|planned| planned.accessor == accessor) {
            return Err(EmitError::AccessorCollision {
                type_name: def.name.clone(),
                accessor,
            });
        }
        fields.push(PlannedField {
            key: field.key.clone(),
            accessor,
            ty: plan_ty(&field.ty, &def.name, &field.key)?,
        });
    }
    Ok(PlannedType {
        name: def.name.clone(),
        fields,
    })
}

/// Lifts a field's optionality out and checks nothing below it is optional too.
fn plan_ty(ty: &Ty, type_name: &str, key: &str) -> Result<PlannedTy, EmitError> {
    let (optional, inner) = match ty {
        Ty::Optional(inner) => (true, inner.as_ref()),
        other => (false, other),
    };
    if contains_optional(inner) {
        return Err(EmitError::MisplacedOptional {
            type_name: type_name.to_string(),
            key: key.to_string(),
        });
    }
    check_supported(inner, type_name, key)?;
    Ok(PlannedTy {
        optional,
        inner: inner.clone(),
    })
}

/// True if `ty` has an `optional` anywhere inside it.
///
/// Iterative, for the reason every walk in this project is: the walk that finds
/// a pathological schema must not itself be pathological.
pub(crate) fn contains_optional(ty: &Ty) -> bool {
    let mut pending = vec![ty];
    while let Some(node) = pending.pop() {
        match node {
            Ty::Optional(_) => return true,
            Ty::List(inner) | Ty::Map(inner) => pending.push(inner),
            _ => {}
        }
    }
    false
}

/// Rejects a [`Ty`] arm this emitter has no spelling for.
///
/// `Ty` is `#[non_exhaustive]`, so this cannot be a compile-time exhaustiveness
/// check here; Phase 4b's new arms land as a runtime rejection naming the field
/// rather than as generated source that does not compile.
pub(crate) fn check_supported(ty: &Ty, type_name: &str, key: &str) -> Result<(), EmitError> {
    let mut pending = vec![ty];
    while let Some(node) = pending.pop() {
        match node {
            Ty::Bool | Ty::Int | Ty::Float | Ty::Str | Ty::Bytes | Ty::Dynamic | Ty::Named(_) => {}
            Ty::Optional(inner) | Ty::List(inner) | Ty::Map(inner) => pending.push(inner),
            _ => {
                return Err(EmitError::UnsupportedType {
                    type_name: type_name.to_string(),
                    key: key.to_string(),
                });
            }
        }
    }
    Ok(())
}

/// Expands every struct-typed path reachable from the root into a class.
///
/// Breadth-first so the emitted order is a function of the schema alone, and
/// capped so a diamond-shaped schema cannot expand without bound.
fn plan_classes(root: &str, types: &[PlannedType]) -> Result<Vec<PlannedClass>, EmitError> {
    let mut classes: Vec<PlannedClass> = Vec::new();
    let mut pending: Vec<Pending> = vec![Pending {
        stem: root.to_string(),
        type_name: root.to_string(),
        path: String::new(),
        optional: false,
    }];
    let mut next = 0usize;

    while next < pending.len() {
        let Pending {
            stem,
            type_name,
            path,
            optional,
        } = pending[next].clone();
        next += 1;
        if classes.len() >= MAX_CLASSES {
            return Err(EmitError::TooManyClasses { max: MAX_CLASSES });
        }
        let Some(def) = types.iter().find(|t| t.name == type_name) else {
            // `Schema` already rejected a dangling `Ty::Named`, so this is
            // unreachable through `Plan::build`; skipping keeps it total.
            continue;
        };
        let accessors = plan_accessors(&stem, &path, def, &mut pending)?;
        classes.push(PlannedClass {
            name: class_name(&stem),
            type_name,
            path,
            optional,
            accessors,
        });
    }
    Ok(classes)
}

/// Names and addresses every field of `def` as reached at `path`, queueing each
/// struct-typed one for a class of its own.
fn plan_accessors(
    stem: &str,
    path: &str,
    def: &PlannedType,
    pending: &mut Vec<Pending>,
) -> Result<Vec<PlannedAccessor>, EmitError> {
    let mut accessors = Vec::with_capacity(def.fields.len());
    for field in &def.fields {
        let child_path = join(path, &field.key);
        validate_path(&child_path, path, &field.key)?;
        let nested = match &field.ty.inner {
            Ty::Named(name) => {
                let child_stem = format!("{stem}{}", upper_camel(&field.key));
                pending.push(Pending {
                    stem: child_stem.clone(),
                    type_name: name.clone(),
                    path: child_path.clone(),
                    optional: field.ty.optional,
                });
                Some(class_name(&child_stem))
            }
            _ => None,
        };
        accessors.push(PlannedAccessor {
            key: field.key.clone(),
            accessor: field.accessor.clone(),
            path: child_path,
            ty: field.ty.clone(),
            nested,
        });
    }
    Ok(accessors)
}

/// One struct-typed path waiting to be expanded into a class.
#[derive(Clone)]
struct Pending {
    /// What the class's name is built from.
    stem: String,
    /// The schema type at this path.
    type_name: String,
    /// The wire path of the struct itself.
    path: String,
    /// True when the field that reached it was optional.
    optional: bool,
}

/// `<stem>Client`.
fn class_name(stem: &str) -> String {
    format!("{stem}Client")
}

/// `<Root>Commands` — the one class the command half of the plan emits.
///
/// A function rather than a `format!` at each site so the Dart emitter, the
/// TypeScript emitter and the collision check cannot disagree about it.
pub(crate) fn commands_class_name(root: &str) -> String {
    format!("{root}Commands")
}

/// Appends one key to a wire path.
fn join(prefix: &str, key: &str) -> String {
    if prefix.is_empty() {
        key.to_string()
    } else {
        format!("{prefix}.{key}")
    }
}

/// Runs a minted path through the **real** parser and checks it comes back as
/// the keys it was built from.
///
/// This is the check that makes "every path is a compile-time literal" mean
/// something. A key that renders into a string parsing back as a different path
/// — one segment in, two out — is exactly the hazard
/// [`Path::from_segments`](duet_core::Path::from_segments) documents and only a
/// `debug_assert!` catches. Here it is a codegen-time rejection, against the
/// parser itself rather than against a re-derivation of its grammar.
fn validate_path(path: &str, prefix: &str, key: &str) -> Result<(), EmitError> {
    let unspellable = |because| EmitError::UnspellableKey {
        type_name: format!("the struct at path \"{prefix}\""),
        key: key.to_string(),
        because,
    };
    let parsed = Path::parse(path).map_err(|_| unspellable("it does not parse as a wire path"))?;
    let last = parsed.segments().last();
    match last {
        Some(Segment::Key(parsed_key)) if parsed_key == key => Ok(()),
        _ => Err(unspellable(
            "it does not survive a round trip through the path parser",
        )),
    }
}

/// Rejects two generated declarations wanting one name.
///
/// The three families share one namespace in Dart — a data class, its codec
/// class, and an accessor class are all top-level classes — so a schema type
/// called `AppEditorClient` and the accessor class for `App.editor` would
/// collide and one would silently win.
fn check_declaration_names(plan: &Plan) -> Result<(), EmitError> {
    let mut seen: Vec<&str> = Vec::with_capacity(plan.types.len() * 2 + plan.classes.len());
    let codecs: Vec<String> = plan
        .types
        .iter()
        .map(|t| format!("{}Codec", t.name))
        .collect();
    // The commands class is one more top-level declaration in the same
    // namespace, and it only exists when the schema declares commands — so a
    // schema type called `AppCommands` is a collision only when there is
    // something for it to collide with.
    let commands_class = commands_class_name(&plan.root);
    let emitted_commands_class = (!plan.commands.is_empty()).then_some(commands_class.as_str());
    let names = plan
        .types
        .iter()
        .map(|t| t.name.as_str())
        .chain(codecs.iter().map(String::as_str))
        .chain(plan.classes.iter().map(|c| c.name.as_str()))
        .chain(emitted_commands_class);
    for name in names {
        if seen.contains(&name) {
            return Err(EmitError::DeclarationCollision {
                name: name.to_string(),
            });
        }
        seen.push(name);
    }
    Ok(())
}

#[cfg(test)]
#[path = "plan_tests.rs"]
mod tests;
