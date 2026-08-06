//! Wire keys: where they come from, and the three ways two of them can be wrong
//! together.
//!
//! # Checked here rather than at `Schema::of`
//!
//! `Schema::of` already refuses a duplicate key, and
//! `duet-codegen` already refuses two keys that camel-case alike. Both are
//! **runtime** failures of a program that compiled, and both name schema keys
//! rather than the Rust fields that produced them. Refusing them here turns
//! each into a `cargo check` error pointing at the second of the two fields,
//! which is the edit that has to change.
//!
//! # What is deliberately *not* checked here
//!
//! The emitters refuse more than this: a key that is not ASCII, one that spells
//! a Dart reserved word, one that starts with a digit. Those are properties of
//! the two **target languages**, not of the schema — `schema/unemittable/` holds
//! valid schemas the emitters refuse — and a Rust host may legitimately share a
//! type it never generates a guest client for. They stay where the knowledge is,
//! in `duet generate`, and they name a single field with an obvious fix.
//!
//! The two checked here are the ones that make two **distinct Rust fields
//! indistinguishable downstream**: after either, one of the two fields is
//! unreachable and nothing can say which one the developer meant.

use duet_codegen::name::lower_camel;
use duet_core::{Path, Segment};
use proc_macro2::Span;

use crate::errors::Errors;
use crate::model::{Field, Role, unraw};

/// Settles one field's wire key: its `rename`, or its name.
pub fn role(ident: &syn::Ident, rename: Option<&syn::LitStr>) -> Role {
    match rename {
        Some(literal) => Role::Shared {
            key: literal.value(),
            span: literal.span(),
        },
        None => Role::Shared {
            key: unraw(ident),
            span: ident.span(),
        },
    }
}

/// Runs every key check over one struct's fields.
pub fn check(fields: &[Field], errors: &mut Errors) {
    let legal: Vec<(&str, Span)> = fields
        .iter()
        .filter_map(|field| match &field.role {
            Role::Shared { key, span } => Some((key.as_str(), *span)),
            Role::Skipped => None,
        })
        .filter(|(key, span)| check_grammar(key, *span, errors))
        .collect();
    check_duplicates(&legal, errors);
    check_camel_collisions(&legal, errors);
}

/// True if `key` survives being rendered into a path string and parsed back as
/// exactly one key segment holding exactly `key`.
///
/// Run against the real [`Path::parse`] rather than against a re-derivation of
/// its grammar, for the reason
/// [`Path::from_segments`](duet_core::Path::from_segments) spells out: the check
/// there is a `debug_assert!` compiled out of release builds, and a key that
/// does not round-trip produces a path literal addressing a *different* node —
/// one segment in, two out.
fn round_trips(key: &str) -> bool {
    matches!(
        Path::parse(key).as_ref().map(Path::segments),
        Ok([Segment::Key(parsed)]) if parsed == key
    )
}

/// Reports a key that is not one path segment, and says so.
fn check_grammar(key: &str, span: Span, errors: &mut Errors) -> bool {
    if round_trips(key) {
        return true;
    }
    errors.push(syn::Error::new(
        span,
        format!(
            "`{key}` is not a legal wire key: it has to be exactly one path segment.\n\
             A key is one step of every path that addresses this field, so it may not be empty \
             and may not contain `.`, `[` or `]` — `editor.zoom` written as a key would be read \
             back as two segments addressing something else entirely.\n\
             Fix: choose a key made only of characters a path segment allows, with \
             `#[duet(rename = \"...\")]`."
        ),
    ));
    false
}

/// Reports two fields renamed onto one key.
///
/// They would occupy one entry in one `Value::Map`: one of the two is
/// unreadable, and which one is not something the developer chose.
fn check_duplicates(keys: &[(&str, Span)], errors: &mut Errors) {
    for (position, (key, span)) in keys.iter().enumerate() {
        if !keys[..position].iter().any(|(earlier, _)| earlier == key) {
            continue;
        }
        errors.push(syn::Error::new(
            *span,
            format!(
                "two fields share the wire key `{key}`.\n\
                 A struct is one `Value::Map`, so both would occupy one entry: one of the two \
                 would be unreachable, and nothing here can know which.\n\
                 Fix: give one of them a different key with `#[duet(rename = \"...\")]`, or \
                 `#[duet(skip)]` the one that is not shared state."
            ),
        ));
    }
}

/// Reports two keys that become one Dart and TypeScript accessor.
///
/// `font_size` and `fontSize` are two distinct wire keys and two distinct store
/// nodes, but `duet-codegen` gives both the accessor `fontSize` — underscores
/// are separators in the casing rule and simply disappear. The generated client
/// would have two members of one name, and nothing here can know which of the
/// two the developer meant to reach.
fn check_camel_collisions(keys: &[(&str, Span)], errors: &mut Errors) {
    for (position, (key, span)) in keys.iter().enumerate() {
        let accessor = lower_camel(key);
        let Some((earlier, _)) = keys[..position]
            .iter()
            .find(|(earlier, _)| *earlier != *key && lower_camel(earlier) == accessor)
        else {
            continue;
        };
        errors.push(syn::Error::new(
            *span,
            format!(
                "`{earlier}` and `{key}` are different wire keys that both become the accessor \
                 `{accessor}`.\n\
                 A generated Dart or TypeScript client names members in `lowerCamelCase` while \
                 leaving the path spelled exactly as the schema does, so these two fields would \
                 collide on one member while still addressing two different nodes.\n\
                 Fix: rename one of them with `#[duet(rename = \"...\")]` so the two accessors \
                 differ."
            ),
        ));
    }
}

#[cfg(test)]
#[path = "keys_tests.rs"]
mod tests;
