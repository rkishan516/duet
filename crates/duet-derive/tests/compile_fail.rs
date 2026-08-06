//! One compile-fail case per rejection, with the message committed.
//!
//! # Two kinds of rejection, one directory
//!
//! `tests/ui/` mixes them on purpose, because a developer meets them the same
//! way:
//!
//! - **The absence of an impl.** `u64`, `f32`, `HashSet`, a non-`String`-keyed
//!   map, a borrow, `Rc`/`RefCell`, `PathBuf`, `Duration`, `Option<Option<T>>`.
//!   The derive never looks at these — it writes `<FieldTy as SharedState>::…`
//!   and the compiler answers. That is what makes `type Blob = Vec<u8>;`
//!   harmless: an alias resolves before trait selection, and there is no token
//!   for a syntactic special case to have matched.
//! - **The derive's own refusals.** A `rename` that is not one path segment,
//!   two fields on one key, two keys that camel-case alike, `skip` without
//!   `Default`, an enum, a union, a tuple struct, a generic type, an
//!   unrecognised attribute.
//!
//! # `.stderr` files are brittle across compilers, and how that is handled
//!
//! These files record **rustc's rendering**, not just Duet's text: a change to
//! how the compiler frames a trait error moves them without anything being
//! wrong. The files committed here were generated with:
//!
//! ```text
//! rustc 1.92.0 (ded5c06cf 2025-12-08)
//! ```
//!
//! A toolchain bump that reformats a diagnostic fails
//! `every_rejection_is_refused_with_the_message_committed_for_it`, and
//! `trybuild` prints the command to fix it. Regenerate with:
//!
//! ```text
//! TRYBUILD=overwrite cargo test -p duet-derive --test compile_fail
//! ```
//!
//! then **read the diff** — which is the step regeneration makes easy to skip,
//! and the reason the second test in this file exists. It reads the committed
//! `.stderr` files as text, with no compiler involved, and asserts each still
//! contains the words that name the fix. A regeneration that quietly dropped
//! Duet's guidance, or a diagnostic that stopped reaching the user, fails
//! there — on a check no toolchain bump can move.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// Every case, and the words its committed message must still contain.
///
/// Not a restatement of the whole message: what is pinned is the **fix**, since
/// that is the part a rewording is most likely to lose and the part a developer
/// meeting the error actually needs. `duet-schema`'s `on_unimplemented` notes
/// supply the first nine; the derive's own `compile_error!`s supply the rest.
const NAMES_THE_FIX: &[(&str, &[&str])] = &[
    (
        "u64",
        &["cannot be shared through a Duet store", "->  `i64`"],
    ),
    ("f32", &["->  `f64`"]),
    ("hash_set", &["`HashSet<T>`", "->  `Vec<T>`"]),
    ("non_string_map", &["->  `HashMap<String, V>`"]),
    ("borrowed", &["->  the owned type"]),
    ("rc_refcell", &["-> the inner type"]),
    ("path_buf", &["`PathBuf` `OsString`", "->  `String`"]),
    (
        "duration",
        &["an explicit `i64` or `String` encoding you choose"],
    ),
    (
        "nested_option",
        &["flatten `Option<Option<T>>` to `Option<T>`"],
    ),
    ("bad_rename", &["not a legal wire key", "Fix: choose a key"]),
    ("duplicate_key", &["Fix: give one of them a different key"]),
    ("camel_collision", &["Fix: rename one of them"]),
    (
        "skip_without_default",
        &["must implement `Default`", "add `#[derive(Default)]`"],
    ),
    ("on_an_enum", &["this is an enum", "by hand"]),
    ("on_a_union", &["this is a union", "by hand"]),
    (
        "on_a_tuple_struct",
        &["this is a tuple struct", "named fields"],
    ),
    (
        "generics",
        &["cannot describe a generic type", "concrete struct"],
    ),
    (
        "unknown_attribute",
        &["is not a Duet attribute", "the three are"],
    ),
];

/// Where the cases live.
fn ui_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("ui")
}

/// Every case's stem, from the directory rather than from the table.
fn cases() -> BTreeSet<String> {
    fs::read_dir(ui_dir())
        .unwrap_or_else(|e| panic!("cannot list tests/ui: {e}"))
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|e| e == "rs"))
        .filter_map(|path| path.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .collect()
}

#[test]
fn every_rejection_is_refused_with_the_message_committed_for_it() {
    // The exact check, and the brittle one. See this file's documentation for
    // what to do when a toolchain bump moves it.
    let cases = trybuild::TestCases::new();
    cases.compile_fail("tests/ui/*.rs");
}

#[test]
fn every_case_has_a_committed_message_and_an_entry_in_the_table() {
    // A case with no `.stderr` is one `trybuild` would *create* on its first
    // run and then agree with forever, which is a golden file that blessed
    // itself. A case with no table entry is one whose guidance nothing checks.
    let tabled: BTreeSet<String> = NAMES_THE_FIX
        .iter()
        .map(|(stem, _)| (*stem).to_string())
        .collect();
    assert_eq!(cases(), tabled, "tests/ui and the table disagree");

    for stem in cases() {
        let expected = ui_dir().join(format!("{stem}.stderr"));
        assert!(
            expected.is_file(),
            "{} has no committed message. Generate it with:\n    \
             TRYBUILD=overwrite cargo test -p duet-derive --test compile_fail",
            expected.display()
        );
    }
}

#[test]
fn every_committed_message_still_names_the_fix() {
    // Reads the files as text, with no compiler involved, so this survives
    // every toolchain bump — and fails whenever a regeneration drops the part
    // of a diagnostic that says what to do instead.
    for (stem, phrases) in NAMES_THE_FIX {
        let path = ui_dir().join(format!("{stem}.stderr"));
        let message = fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
        for phrase in *phrases {
            assert!(
                message.contains(phrase),
                "{stem}.stderr no longer says {phrase:?}, so the developer meeting this \
                 rejection is not being told what to do instead:\n{message}"
            );
        }
    }
}
