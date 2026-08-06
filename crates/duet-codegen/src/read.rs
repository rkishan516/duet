//! A `serde_json` reader for the rendered schema format.
//!
//! # Why this is not the writer run backwards
//!
//! [`Schema::render`] is a hand-rolled writer that pushes literal strings into
//! a `String`. This reader walks a `serde_json::Value` tree and matches on
//! literal strings. The two share no code and no dependency: `duet-schema` does
//! not depend on `serde_json` at all, which is what makes the independence a
//! property of the build graph rather than a promise.
//!
//! That is what turns `render → parse → compare` into a cross-check. It catches
//! the whole class of *asymmetric* bugs — an escape the writer emits and the
//! reader cannot read, a sort the writer forgot, a nesting the reader
//! mis-associates, a key one side spells differently — because two unrelated
//! code paths have to agree on the bytes in between. What it cannot catch is a
//! shared misunderstanding of what the format *should* be; the hand-written
//! fixtures, the negative fixtures and the real-host path check exist for that.
//!
//! # Totality
//!
//! A schema file is an input like any other. Nothing here panics, allocates
//! without bound, or recurses without bound:
//!
//! - `serde_json`'s **default** recursion limit is left on, so a document
//!   nested past it is a parse error rather than a stack overflow.
//! - The type walk carries an explicit depth, checked before it descends, so it
//!   is bounded even inside that limit.
//! - Every object's key set is checked exactly, so an unknown key is a
//!   rejection rather than a silently different meaning.

use duet_schema::{FieldDef, SCHEMA_VERSION, Schema, Ty, TypeDef};
use serde_json::{Map, Value as Json};

use crate::error::ReadError;

/// The most type constructors one field may nest.
///
/// Comfortably past [`MAX_VALUE_DEPTH`](duet_core::MAX_VALUE_DEPTH), which
/// bounds what the store can hold, so this never rejects a schema the store
/// would have accepted; it exists to bound the *walk*, not the schema.
pub const MAX_TY_DEPTH: usize = 64;

/// Every `"kind"` this reader accepts, in the order [`Ty`]'s arms are declared.
///
/// The single list of what the schema's type language contains. The coverage
/// floor in `tests/coverage.rs` asserts every entry appears in a committed
/// fixture, so a new [`Ty`] arm cannot reach the emitters without one.
pub const KINDS: &[&str] = &[
    "bool", "int", "float", "string", "bytes", "dynamic", "optional", "list", "map", "named",
];

/// Parses a rendered schema document.
///
/// # Errors
///
/// [`ReadError`] for a document that is not JSON, is not in this format, or
/// describes a schema [`Schema::build`] refuses. Never panics, whatever the
/// input.
pub fn read_schema(text: &str) -> Result<Schema, ReadError> {
    let document: Json = serde_json::from_str(text)?;
    let root_object = object(&document, "the schema")?;
    exact_keys(root_object, "the schema", &["root", "types", "version"])?;
    check_version(root_object)?;

    let root = read_ty(require(root_object, "the schema", "root")?, "root", 0)?;
    let types = read_types(require(root_object, "the schema", "types")?)?;
    Schema::build(root, types).map_err(ReadError::Invalid)
}

/// Checks the document's declared format version against this reader's.
fn check_version(document: &Map<String, Json>) -> Result<(), ReadError> {
    let found =
        require(document, "the schema", "version")?
            .as_u64()
            .ok_or(ReadError::WrongKind {
                at: "\"version\"".to_string(),
                expected: "a non-negative integer",
            })?;
    if found == u64::from(SCHEMA_VERSION) {
        Ok(())
    } else {
        Err(ReadError::UnsupportedVersion {
            found,
            expected: SCHEMA_VERSION,
        })
    }
}

/// Reads the `types` array.
fn read_types(node: &Json) -> Result<Vec<TypeDef>, ReadError> {
    let entries = node.as_array().ok_or(ReadError::WrongKind {
        at: "\"types\"".to_string(),
        expected: "an array",
    })?;
    entries
        .iter()
        .enumerate()
        .map(|(index, entry)| read_type_def(entry, &format!("types[{index}]")))
        .collect()
}

/// Reads one `{"fields": [...], "name": "..."}`.
fn read_type_def(node: &Json, at: &str) -> Result<TypeDef, ReadError> {
    let def = object(node, at)?;
    exact_keys(def, at, &["fields", "name"])?;
    let name = string(require(def, at, "name")?, &format!("{at}.name"))?;
    let fields = require(def, at, "fields")?
        .as_array()
        .ok_or_else(|| ReadError::WrongKind {
            at: format!("{at}.fields"),
            expected: "an array",
        })?;
    let fields = fields
        .iter()
        .enumerate()
        .map(|(index, field)| read_field(field, &format!("{at}.fields[{index}]")))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(TypeDef {
        name: name.to_string(),
        fields,
    })
}

/// Reads one `{"key": "...", "type": {...}}`.
fn read_field(node: &Json, at: &str) -> Result<FieldDef, ReadError> {
    let field = object(node, at)?;
    exact_keys(field, at, &["key", "type"])?;
    let key = string(require(field, at, "key")?, &format!("{at}.key"))?;
    let ty = read_ty(require(field, at, "type")?, &format!("{at}.type"), 0)?;
    Ok(FieldDef::new(key, ty))
}

/// Reads one tagged type node.
///
/// `depth` counts the constructors already entered and is checked **before**
/// descending, so the recursion is bounded by [`MAX_TY_DEPTH`] however the
/// document nests within `serde_json`'s own parse limit.
fn read_ty(node: &Json, at: &str, depth: usize) -> Result<Ty, ReadError> {
    if depth > MAX_TY_DEPTH {
        return Err(ReadError::TooDeep {
            at: at.to_string(),
            max: MAX_TY_DEPTH,
        });
    }
    let ty = object(node, at)?;
    let kind = string(require(ty, at, "kind")?, &format!("{at}.kind"))?;
    match kind {
        "bool" => scalar(ty, at, Ty::Bool),
        "int" => scalar(ty, at, Ty::Int),
        "float" => scalar(ty, at, Ty::Float),
        "string" => scalar(ty, at, Ty::Str),
        "bytes" => scalar(ty, at, Ty::Bytes),
        "dynamic" => scalar(ty, at, Ty::Dynamic),
        "optional" => Ok(wrapped(ty, at, depth)?.optional()),
        "list" => Ok(wrapped(ty, at, depth)?.list()),
        "map" => Ok(wrapped(ty, at, depth)?.map()),
        "named" => {
            exact_keys(ty, at, &["kind", "name"])?;
            let name = string(require(ty, at, "name")?, &format!("{at}.name"))?;
            Ok(Ty::Named(name.to_string()))
        }
        other => Err(ReadError::UnknownKind {
            at: at.to_string(),
            kind: other.to_string(),
        }),
    }
}

/// A kind that carries nothing but its tag.
fn scalar(ty: &Map<String, Json>, at: &str, arm: Ty) -> Result<Ty, ReadError> {
    exact_keys(ty, at, &["kind"])?;
    Ok(arm)
}

/// The `of` of a kind that wraps one other type.
fn wrapped(ty: &Map<String, Json>, at: &str, depth: usize) -> Result<Ty, ReadError> {
    exact_keys(ty, at, &["kind", "of"])?;
    read_ty(require(ty, at, "of")?, &format!("{at}.of"), depth + 1)
}

/// `node` as a JSON object.
fn object<'a>(node: &'a Json, at: &str) -> Result<&'a Map<String, Json>, ReadError> {
    node.as_object().ok_or_else(|| ReadError::WrongKind {
        at: at.to_string(),
        expected: "an object",
    })
}

/// `node` as a JSON string.
fn string<'a>(node: &'a Json, at: &str) -> Result<&'a str, ReadError> {
    node.as_str().ok_or_else(|| ReadError::WrongKind {
        at: at.to_string(),
        expected: "a string",
    })
}

/// The value at `key`, or [`ReadError::MissingKey`].
fn require<'a>(
    object: &'a Map<String, Json>,
    at: &str,
    key: &'static str,
) -> Result<&'a Json, ReadError> {
    object.get(key).ok_or_else(|| ReadError::MissingKey {
        at: at.to_string(),
        key,
    })
}

/// Rejects any key the format does not define.
///
/// The `MissingKey` half is left to [`require`], so a message names the one key
/// that is actually wanted rather than reciting the whole set.
fn exact_keys(
    object: &Map<String, Json>,
    at: &str,
    allowed: &[&'static str],
) -> Result<(), ReadError> {
    for key in object.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(ReadError::UnexpectedKey {
                at: at.to_string(),
                key: key.clone(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "read_tests.rs"]
mod tests;
