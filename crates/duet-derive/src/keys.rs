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

/// What a set of keys belongs to.
///
/// The three checks are identical for both; the **messages** are not, because a
/// message that names the wrong thing sends a developer to the wrong file, and
/// one that offers `#[duet(skip)]` to someone writing a command offers an
/// attribute that does not exist there.
#[derive(Clone, Copy)]
pub enum Subject {
    /// A struct's fields, under `#[derive(SharedState)]`.
    Field,
    /// A command's arguments, under `#[command]`.
    Argument,
}

impl Subject {
    /// The plural noun a message calls two of these.
    fn plural(self) -> &'static str {
        match self {
            Subject::Field => "fields",
            Subject::Argument => "arguments",
        }
    }

    /// Why a key must be exactly one path segment, for this subject.
    fn why_one_segment(self) -> &'static str {
        match self {
            Subject::Field => {
                "A key is one step of every path that addresses this field, so it may not be \
                 empty and may not contain `.`, `[` or `]` — `editor.zoom` written as a key \
                 would be read back as two segments addressing something else entirely."
            }
            Subject::Argument => {
                "A key is the name an argument arrives under and the name a generated client \
                 gives the parameter, so it may not be empty and may not contain `.`, `[` or \
                 `]` — neither Dart nor TypeScript has an identifier containing one."
            }
        }
    }

    /// The map two keys would collide inside, and the way out.
    fn why_one_entry(self) -> &'static str {
        match self {
            Subject::Field => {
                "A struct is one `Value::Map`, so both would occupy one entry: one of the two \
                 would be unreachable, and nothing here can know which.\n\
                 Fix: give one of them a different key with `#[duet(rename = \"...\")]`, or \
                 `#[duet(skip)]` the one that is not shared state."
            }
            Subject::Argument => {
                "An invocation's `args` is one `Value::Map`, so both would occupy one entry: one \
                 of the two could never be supplied, and nothing here can know which.\n\
                 Fix: give one of them a different key with `#[duet(rename = \"...\")]`, or \
                 delete the one that is not an argument."
            }
        }
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
        .collect();
    check_all(&legal, Subject::Field, errors);
}

/// Runs every key check over a list of `(key, where to complain)` pairs.
///
/// Shared by `#[derive(SharedState)]`'s fields and `#[command]`'s arguments,
/// because the three hazards are the same three: a key that is not one path
/// segment, two keys that are equal, and two keys that camel-case alike. A
/// command's arguments arrive in a `Value::Map` exactly as a struct's fields do
/// and are named in a generated client exactly as a struct's fields are, so a
/// second copy of these rules would be a second thing to keep in step — and the
/// one that fell behind would be the one nobody was reading.
pub fn check_all(keys: &[(&str, Span)], subject: Subject, errors: &mut Errors) {
    let legal: Vec<(&str, Span)> = keys
        .iter()
        .copied()
        .filter(|(key, span)| check_grammar(key, *span, subject, errors))
        .collect();
    check_duplicates(&legal, subject, errors);
    check_camel_collisions(&legal, subject, errors);
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
fn check_grammar(key: &str, span: Span, subject: Subject, errors: &mut Errors) -> bool {
    if round_trips(key) {
        return true;
    }
    errors.push(syn::Error::new(
        span,
        format!(
            "`{key}` is not a legal wire key: it has to be exactly one path segment.\n\
             {}\n\
             Fix: choose a key made only of characters a path segment allows, with \
             `#[duet(rename = \"...\")]`.",
            subject.why_one_segment()
        ),
    ));
    false
}

/// Reports two fields renamed onto one key.
///
/// They would occupy one entry in one `Value::Map`: one of the two is
/// unreadable, and which one is not something the developer chose.
fn check_duplicates(keys: &[(&str, Span)], subject: Subject, errors: &mut Errors) {
    for (position, (key, span)) in keys.iter().enumerate() {
        if !keys[..position].iter().any(|(earlier, _)| earlier == key) {
            continue;
        }
        errors.push(syn::Error::new(
            *span,
            format!(
                "two {} share the wire key `{key}`.\n{}",
                subject.plural(),
                subject.why_one_entry()
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
fn check_camel_collisions(keys: &[(&str, Span)], subject: Subject, errors: &mut Errors) {
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
                 leaving the path spelled exactly as the schema does, so these two {} would \
                 collide on one member while still addressing two different nodes.\n\
                 Fix: rename one of them with `#[duet(rename = \"...\")]` so the two accessors \
                 differ.",
                subject.plural()
            ),
        ));
    }
}

#[cfg(test)]
#[path = "keys_tests.rs"]
mod tests;
