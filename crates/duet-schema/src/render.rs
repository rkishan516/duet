//! The hand-rolled JSON writer for [`Schema`].
//!
//! # Why this is not `serde_json`
//!
//! `crates/duet-codegen` (increment 4) reads a schema back with a `serde_json`
//! **reader**. If this were a `serde_json` writer, a write-then-read round trip
//! would be checking `serde_json` against itself — a tautology that would pass
//! just as happily if the schema format were wrong. An independent writer and
//! reader make that round trip a genuine cross-check, and keeping this crate
//! free of `serde_json` is what guarantees the independence rather than merely
//! asserting it.
//!
//! # Determinism is a requirement, not a nicety
//!
//! The rendered schema is committed, diffed and compared byte-for-byte: a
//! stale-schema check fails CI, and `#[derive(SharedState)]` has to produce a
//! file byte-identical to a hand-written one. So: object keys are emitted in sorted
//! order, `types` is sorted by name, indentation is fixed at two spaces, and
//! the file ends with exactly one newline. No floats are ever emitted, so no
//! float formatting can vary.

use std::fmt::Write as _;

use crate::schema::Schema;
use crate::ty::{Ty, TypeDef};

/// The schema format's version, emitted as `"version"`.
///
/// Bumped only when the *shape* of the rendered document changes, so a reader
/// meeting a version it does not know can refuse rather than misread. Adding a
/// new [`Ty`] arm is not a shape change: a reader that does not know the arm
/// already has to reject it by name.
pub const SCHEMA_VERSION: u32 = 1;

impl Schema {
    /// Renders this schema as deterministic, byte-stable JSON.
    ///
    /// The output ends with a newline so the file is a well-formed text file
    /// and a diff of it never reports "no newline at end of file".
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str("{\n");
        out.push_str("  \"root\": ");
        write_ty(&mut out, self.root(), 1);
        out.push_str(",\n  \"types\": ");
        write_types(&mut out, self.types(), 1);
        // `write!` to a `String` is infallible; the `Result` is discarded rather
        // than unwrapped so this module keeps its no-panic property.
        let _ = write!(out, ",\n  \"version\": {SCHEMA_VERSION}\n}}\n");
        out
    }
}

/// Two spaces per level, matching the fixed indentation of the rest.
fn indent(out: &mut String, level: usize) {
    for _ in 0..level {
        out.push_str("  ");
    }
}

/// Writes the `types` array, one object per definition, sorted by name.
fn write_types(out: &mut String, types: &[TypeDef], level: usize) {
    if types.is_empty() {
        out.push_str("[]");
        return;
    }
    out.push_str("[\n");
    for (position, def) in types.iter().enumerate() {
        indent(out, level + 1);
        write_type_def(out, def, level + 1);
        if position + 1 < types.len() {
            out.push(',');
        }
        out.push('\n');
    }
    indent(out, level);
    out.push(']');
}

/// Writes one definition: `fields` then `name`, sorted.
fn write_type_def(out: &mut String, def: &TypeDef, level: usize) {
    out.push_str("{\n");
    indent(out, level + 1);
    out.push_str("\"fields\": ");
    write_fields(out, def, level + 1);
    out.push_str(",\n");
    indent(out, level + 1);
    out.push_str("\"name\": ");
    write_string(out, &def.name);
    out.push('\n');
    indent(out, level);
    out.push('}');
}

/// Writes a definition's fields in **declaration order**.
///
/// The one place ordering is not sorted, and deliberately so: field order is
/// what generated positional constructors in Dart and TypeScript use, so it is
/// part of the contract and must follow the Rust declaration rather than an
/// alphabet.
fn write_fields(out: &mut String, def: &TypeDef, level: usize) {
    if def.fields.is_empty() {
        out.push_str("[]");
        return;
    }
    out.push_str("[\n");
    for (position, field) in def.fields.iter().enumerate() {
        indent(out, level + 1);
        out.push_str("{\"key\": ");
        write_string(out, &field.key);
        out.push_str(", \"type\": ");
        write_ty(out, &field.ty, level + 1);
        out.push('}');
        if position + 1 < def.fields.len() {
            out.push(',');
        }
        out.push('\n');
    }
    indent(out, level);
    out.push(']');
}

/// Writes one [`Ty`] as a tagged object.
///
/// Every arm's keys are already in sorted order by construction — `kind` before
/// `name`, `kind` before `of` — so no arm needs a sort at emit time.
///
/// Written on one line however deeply it nests: a `Ty` is a handful of
/// characters, and putting it on one line keeps a field's whole description
/// greppable as a single line in a golden file.
fn write_ty(out: &mut String, ty: &Ty, level: usize) {
    let scalar = match ty {
        Ty::Bool => "bool",
        Ty::Int => "int",
        Ty::Float => "float",
        Ty::Str => "string",
        Ty::Bytes => "bytes",
        Ty::Dynamic => "dynamic",
        Ty::Optional(inner) => return write_wrapper(out, "optional", inner, level),
        Ty::List(inner) => return write_wrapper(out, "list", inner, level),
        Ty::Map(inner) => return write_wrapper(out, "map", inner, level),
        Ty::Named(name) => {
            out.push_str("{\"kind\": \"named\", \"name\": ");
            write_string(out, name);
            out.push('}');
            return;
        } // No `_` arm, deliberately. `Ty` is `#[non_exhaustive]` for downstream
          // crates but exhaustive here, so adding an arm in Phase 4b fails to
          // compile until it has a spelling — which is what should happen. A
          // catch-all would emit a placeholder kind and let a forgotten update
          // reach a golden file.
    };
    out.push_str("{\"kind\": \"");
    out.push_str(scalar);
    out.push_str("\"}");
}

/// Writes `{"kind": "<kind>", "of": <inner>}`.
fn write_wrapper(out: &mut String, kind: &str, inner: &Ty, level: usize) {
    out.push_str("{\"kind\": \"");
    out.push_str(kind);
    out.push_str("\", \"of\": ");
    write_ty(out, inner, level);
    out.push('}');
}

/// Writes a JSON string literal, escaping exactly what RFC 8259 requires.
///
/// `"` and `\` take their short escapes; the five other short escapes are used
/// where they apply; every remaining control character below `0x20` becomes
/// `\u00xx`. Nothing else is escaped — the output is UTF-8 and a schema's type
/// names and keys may legitimately be non-ASCII, so escaping them would only
/// make the file harder to read without making it more portable.
///
/// A Rust `String` cannot contain an unpaired surrogate, so the surrogate
/// hazard `duet-codec` has to guard against on the wire does not exist here.
fn write_string(out: &mut String, text: &str) {
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0C}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rendered_ty(ty: &Ty) -> String {
        let mut out = String::new();
        write_ty(&mut out, ty, 0);
        out
    }

    fn escaped(text: &str) -> String {
        let mut out = String::new();
        write_string(&mut out, text);
        out
    }

    #[test]
    fn every_scalar_arm_has_a_stable_spelling() {
        assert_eq!(rendered_ty(&Ty::Bool), "{\"kind\": \"bool\"}");
        assert_eq!(rendered_ty(&Ty::Int), "{\"kind\": \"int\"}");
        assert_eq!(rendered_ty(&Ty::Float), "{\"kind\": \"float\"}");
        assert_eq!(rendered_ty(&Ty::Str), "{\"kind\": \"string\"}");
        assert_eq!(rendered_ty(&Ty::Bytes), "{\"kind\": \"bytes\"}");
        assert_eq!(rendered_ty(&Ty::Dynamic), "{\"kind\": \"dynamic\"}");
    }

    #[test]
    fn wrappers_nest_inline() {
        assert_eq!(
            rendered_ty(&Ty::Int.list()),
            "{\"kind\": \"list\", \"of\": {\"kind\": \"int\"}}"
        );
        assert_eq!(
            rendered_ty(&Ty::Str.map().optional()),
            "{\"kind\": \"optional\", \"of\": \
             {\"kind\": \"map\", \"of\": {\"kind\": \"string\"}}}"
        );
    }

    #[test]
    fn a_named_reference_carries_only_its_name() {
        assert_eq!(
            rendered_ty(&Ty::Named("Editor".into())),
            "{\"kind\": \"named\", \"name\": \"Editor\"}"
        );
    }

    #[test]
    fn strings_escape_exactly_what_json_requires() {
        assert_eq!(escaped("plain"), "\"plain\"");
        assert_eq!(escaped("a\"b"), "\"a\\\"b\"");
        assert_eq!(escaped("a\\b"), "\"a\\\\b\"");
        assert_eq!(escaped("a\nb\tc\rd"), "\"a\\nb\\tc\\rd\"");
        assert_eq!(escaped("a\u{08}b\u{0C}c"), "\"a\\bb\\fc\"");
        assert_eq!(escaped("\u{01}\u{1F}"), "\"\\u0001\\u001f\"");
    }

    #[test]
    fn non_ascii_text_is_emitted_as_utf8_not_escaped() {
        // Escaping it would only make the file harder to read: the output is
        // UTF-8 and every reader that matters parses UTF-8.
        assert_eq!(escaped("café🦀"), "\"café🦀\"");
    }

    #[test]
    fn the_control_character_escapes_are_lowercase_hex_and_four_wide() {
        // Pinned because a byte-stable format cannot tolerate `\u1F` or `\u001F`
        // depending on which formatter ran.
        assert_eq!(escaped("\u{1f}"), "\"\\u001f\"");
    }
}
