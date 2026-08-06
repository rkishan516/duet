//! The three ways a set of wire keys can be wrong.

use super::*;
use crate::model::Model;
use proc_macro2::TokenStream;
use quote::quote;

/// The key messages a struct definition produced.
fn problems(tokens: TokenStream) -> Vec<String> {
    match Model::parse(tokens) {
        Ok(_) => Vec::new(),
        Err(combined) => combined.into_iter().map(|e| e.to_string()).collect(),
    }
}

#[test]
fn every_rust_identifier_is_already_a_legal_wire_key() {
    // The derived case can never fail this check — a Rust identifier contains
    // none of `.`, `[`, `]` and is never empty — which is why the message the
    // check carries talks about `rename`.
    for key in ["a", "zoom", "snake_case", "x2", "_private", "type"] {
        assert!(round_trips(key), "{key} should round-trip");
    }
}

#[test]
fn the_path_metacharacters_and_the_empty_key_do_not_round_trip() {
    for key in ["", "a.b", "a[0]", "a]b", "[", "."] {
        assert!(!round_trips(key), "{key} should be refused");
    }
}

#[test]
fn a_rename_that_is_not_one_path_segment_is_refused() {
    let problems = problems(quote! {
        struct App {
            #[duet(rename = "editor.zoom")]
            zoom: f64,
        }
    });
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(
        problems[0].contains("`editor.zoom` is not a legal wire key"),
        "{problems:?}"
    );
    assert!(problems[0].contains("two segments"), "{problems:?}");
}

#[test]
fn an_empty_rename_is_refused() {
    let problems = problems(quote! {
        struct App {
            #[duet(rename = "")]
            zoom: f64,
        }
    });
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains("not a legal wire key"), "{problems:?}");
}

#[test]
fn two_fields_renamed_onto_one_key_are_refused() {
    let problems = problems(quote! {
        struct App {
            title: String,
            #[duet(rename = "title")]
            heading: String,
        }
    });
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(
        problems[0].contains("share the wire key `title`"),
        "{problems:?}"
    );
}

#[test]
fn a_third_field_on_the_same_key_is_reported_once_more_rather_than_missed() {
    let problems = problems(quote! {
        struct App {
            title: String,
            #[duet(rename = "title")]
            heading: String,
            #[duet(rename = "title")]
            caption: String,
        }
    });
    assert_eq!(problems.len(), 2, "{problems:?}");
}

#[test]
fn two_keys_that_camel_case_alike_are_refused_before_the_emitter_sees_them() {
    let problems = problems(quote! {
        struct Editor {
            font_size: i64,
            #[duet(rename = "fontSize")]
            legacy: i64,
        }
    });
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(
        problems[0].contains("`font_size` and `fontSize`"),
        "{problems:?}"
    );
    assert!(problems[0].contains("accessor `fontSize`"), "{problems:?}");
}

#[test]
fn a_leading_underscore_collides_with_the_bare_name() {
    // `lower_camel` treats underscores as separators and drops them, so `_x`
    // and `x` land on one accessor. Deliberately lossy — the emitter reports
    // the collision rather than inventing a distinction, and so does this.
    let problems = problems(quote! {
        struct App {
            #[duet(rename = "_private")]
            hidden: i64,
            #[duet(rename = "private")]
            shown: i64,
        }
    });
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains("accessor `private`"), "{problems:?}");
}

#[test]
fn a_duplicate_key_is_not_also_reported_as_a_camel_collision() {
    // Two keys that are *equal* collide on their accessor too, trivially.
    // Reporting both would name one fault twice and stop localising it.
    let problems = problems(quote! {
        struct App {
            title: String,
            #[duet(rename = "title")]
            heading: String,
        }
    });
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(!problems[0].contains("accessor"), "{problems:?}");
}

#[test]
fn an_illegal_key_is_not_also_reported_as_a_duplicate_or_a_collision() {
    // A key that failed the grammar check is dropped before the pairwise
    // checks run: it is already going to be rewritten, and two complaints about
    // one edit stop pointing at the edit.
    let problems = problems(quote! {
        struct App {
            #[duet(rename = "a.b")]
            first: i64,
            #[duet(rename = "a.b")]
            second: i64,
        }
    });
    assert_eq!(problems.len(), 2, "{problems:?}");
    assert!(
        problems.iter().all(|p| p.contains("not a legal wire key")),
        "{problems:?}"
    );
}

#[test]
fn a_skipped_field_takes_part_in_no_key_check() {
    // Both of the pairwise faults, made unreachable by the skip: `fontSize`
    // would duplicate the rename and collide with `font_size`'s accessor, and
    // it does neither, because it never reaches the wire at all.
    let problems = problems(quote! {
        struct Editor {
            font_size: i64,
            #[duet(skip)]
            fontSize: i64,
        }
    });
    assert!(problems.is_empty(), "{problems:?}");
}
