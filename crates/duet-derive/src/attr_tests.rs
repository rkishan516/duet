//! What each `#[duet(...)]` spelling is understood as, and what every other
//! spelling is refused with.

use super::*;
use proc_macro2::TokenStream;
use quote::{ToTokens, quote};

/// The messages a parse produced, one per problem.
fn problems(errors: Errors) -> Vec<String> {
    match errors.finish(()) {
        Ok(()) => Vec::new(),
        Err(combined) => combined.into_iter().map(|e| e.to_string()).collect(),
    }
}

fn parsed(tokens: TokenStream) -> syn::DeriveInput {
    syn::parse2(tokens).expect("the test input should be a valid item")
}

/// Reads the container attributes of a struct definition.
fn container(tokens: TokenStream) -> (Option<String>, Vec<String>) {
    let input = parsed(tokens);
    let mut errors = Errors::new();
    let found = Container::parse(&input.attrs, &mut errors);
    let krate = found.krate.map(|p| p.to_token_stream().to_string());
    (krate, problems(errors))
}

/// Reads the attributes of a struct's first field.
fn field(tokens: TokenStream) -> (Field, Vec<String>) {
    let input = parsed(tokens);
    let syn::Data::Struct(data) = input.data else {
        unreachable!("the test input is a struct")
    };
    let first = data.fields.iter().next().expect("one field").clone();
    let mut errors = Errors::new();
    let found = Field::parse(&first.attrs, &mut errors);
    (found, problems(errors))
}

#[test]
fn a_container_without_attributes_asks_for_nothing() {
    let (krate, problems) = container(quote! { struct App { counter: i64 } });
    assert_eq!(krate, None);
    assert!(problems.is_empty(), "{problems:?}");
}

#[test]
fn crate_takes_a_path_not_a_string() {
    let (krate, problems) = container(quote! {
        #[duet(crate = ::my_reexport::duet)]
        struct App { counter: i64 }
    });
    assert!(problems.is_empty(), "{problems:?}");
    assert_eq!(krate.as_deref(), Some(":: my_reexport :: duet"));
}

#[test]
fn a_second_crate_is_refused_rather_than_silently_winning() {
    let (_, problems) = container(quote! {
        #[duet(crate = ::one)]
        #[duet(crate = ::two)]
        struct App { counter: i64 }
    });
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(
        problems[0].contains("`crate` is given twice"),
        "{problems:?}"
    );
}

#[test]
fn a_field_attribute_on_the_container_says_where_it_belongs() {
    for attribute in [quote!(rename = "x"), quote!(skip)] {
        let (_, problems) = container(quote! {
            #[duet(#attribute)]
            struct App { counter: i64 }
        });
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(
            problems[0].contains("belongs on a field"),
            "{problems:?} for {attribute}"
        );
    }
}

#[test]
fn crate_on_a_field_says_where_it_belongs() {
    let (_, problems) = field(quote! {
        struct App {
            #[duet(crate = ::duet)]
            counter: i64,
        }
    });
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(
        problems[0].contains("belongs on the struct"),
        "{problems:?}"
    );
}

#[test]
fn rename_takes_a_string_literal() {
    let (found, problems) = field(quote! {
        struct App {
            #[duet(rename = "window_title")]
            title: String,
        }
    });
    assert!(problems.is_empty(), "{problems:?}");
    assert_eq!(
        found.rename.map(|literal| literal.value()).as_deref(),
        Some("window_title")
    );
}

#[test]
fn skip_carries_no_value() {
    let (found, problems) = field(quote! {
        struct App {
            #[duet(skip)]
            cache: Vec<u8>,
        }
    });
    assert!(problems.is_empty(), "{problems:?}");
    assert!(found.skip.is_some());
    assert!(found.rename.is_none());
}

#[test]
fn both_field_attributes_may_be_given_at_once_here() {
    // The *contradiction* between them is settled in `model.rs`, where the
    // field's role is decided. This layer only reports what was written, so
    // that the message about the contradiction can name the `rename` literal.
    let (found, problems) = field(quote! {
        struct App {
            #[duet(skip, rename = "x")]
            cache: Vec<u8>,
        }
    });
    assert!(problems.is_empty(), "{problems:?}");
    assert!(found.skip.is_some() && found.rename.is_some());
}

#[test]
fn a_second_rename_is_refused_rather_than_silently_winning() {
    let (_, problems) = field(quote! {
        struct App {
            #[duet(rename = "one")]
            #[duet(rename = "two")]
            title: String,
        }
    });
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(
        problems[0].contains("`rename` is given twice"),
        "{problems:?}"
    );
}

#[test]
fn an_unrecognised_key_lists_the_three_that_exist() {
    // The case a silent no-op would ship: `renmae` compiling would put the
    // field's Rust name on the wire and nothing would say so.
    let (_, problems) = field(quote! {
        struct App {
            #[duet(renmae = "window_title")]
            title: String,
        }
    });
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(
        problems[0].contains("`renmae` is not a Duet attribute"),
        "{problems:?}"
    );
    assert!(problems[0].contains("`skip`"), "{problems:?}");
}

#[test]
fn a_bare_duet_attribute_says_what_to_write_instead() {
    let (_, problems) = field(quote! {
        struct App {
            #[duet]
            title: String,
        }
    });
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(
        problems[0].contains("says nothing on its own"),
        "{problems:?}"
    );
}

#[test]
fn attributes_of_other_namespaces_are_left_alone() {
    let (found, problems) = field(quote! {
        struct App {
            #[serde(rename = "nope")]
            #[doc = "a field"]
            title: String,
        }
    });
    assert!(problems.is_empty(), "{problems:?}");
    assert!(found.rename.is_none() && found.skip.is_none());
}

#[test]
fn a_malformed_attribute_body_is_reported_and_the_next_one_is_still_read() {
    // `syn`'s nested-meta parser stops at the first error *within* one
    // attribute. Stopping across attributes as well would report one problem
    // per compile, which is the behaviour `duet-schema` deliberately avoids.
    let (_, problems) = field(quote! {
        struct App {
            #[duet(rename = )]
            #[duet(nonsense)]
            title: String,
        }
    });
    assert_eq!(problems.len(), 2, "{problems:?}");
    assert!(
        problems[1].contains("`nonsense` is not a Duet attribute"),
        "{problems:?}"
    );
}
