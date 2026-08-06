//! Which shapes the derive accepts, and what each refusal says.

use super::*;
use proc_macro2::TokenStream;
use quote::{ToTokens, quote};

/// The model a definition produces, or the messages explaining why it does not.
fn read(tokens: TokenStream) -> Result<Model, Vec<String>> {
    Model::parse(tokens).map_err(|combined| combined.into_iter().map(|e| e.to_string()).collect())
}

/// The messages a rejected definition produced.
fn refusal(tokens: TokenStream) -> Vec<String> {
    match read(tokens) {
        Ok(model) => panic!("{} should have been refused", model.name),
        Err(problems) => problems,
    }
}

/// The one model an accepted definition produced.
fn accepted(tokens: TokenStream) -> Model {
    read(tokens).unwrap_or_else(|problems| panic!("should have been accepted: {problems:?}"))
}

#[test]
fn a_struct_with_named_fields_keeps_its_declaration_order() {
    let model = accepted(quote! {
        struct App { counter: i64, editor: Editor, title: String }
    });
    assert_eq!(model.name, "App");
    assert_eq!(model.krate.to_token_stream().to_string(), ":: duet");
    assert_eq!(
        model.fields.iter().map(|f| f.key()).collect::<Vec<_>>(),
        [Some("counter"), Some("editor"), Some("title")]
    );
}

#[test]
fn a_rename_replaces_the_wire_key_and_nothing_else() {
    let model = accepted(quote! {
        struct Window {
            #[duet(rename = "window_title")]
            title: String,
        }
    });
    assert_eq!(model.fields[0].key(), Some("window_title"));
    assert_eq!(model.fields[0].ident.to_string(), "title");
}

#[test]
fn a_skipped_field_has_no_wire_key() {
    let model = accepted(quote! {
        struct App {
            counter: i64,
            #[duet(skip)]
            cache: Vec<u8>,
        }
    });
    assert_eq!(
        model.fields.iter().map(|f| f.key()).collect::<Vec<_>>(),
        [Some("counter"), None]
    );
}

#[test]
fn skip_and_rename_together_are_a_contradiction_rather_than_a_precedence() {
    let problems = refusal(quote! {
        struct App {
            #[duet(skip, rename = "x")]
            cache: Vec<u8>,
        }
    });
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(
        problems[0].contains("contradict each other"),
        "{problems:?}"
    );
}

#[test]
fn a_raw_identifier_loses_its_r_hash_on_the_wire() {
    // `r#` is Rust syntax for "this word is a keyword", not part of the name.
    // Leaving it on would put `r#type` in the store and in every generated Dart
    // accessor.
    let model = accepted(quote! { struct App { r#type: String } });
    assert_eq!(model.fields[0].key(), Some("type"));
    assert_eq!(model.fields[0].ident.to_string(), "r#type");
}

#[test]
fn a_raw_identifier_type_name_loses_its_r_hash_too() {
    let model = accepted(quote! { struct r#struct { counter: i64 } });
    assert_eq!(model.name, "struct");
}

#[test]
fn the_crate_attribute_redirects_every_generated_path() {
    let model = accepted(quote! {
        #[duet(crate = ::my_reexport)]
        struct App { counter: i64 }
    });
    assert_eq!(model.krate.to_token_stream().to_string(), ":: my_reexport");
}

#[test]
fn an_enum_is_refused_and_named_as_one() {
    let problems = refusal(quote! { enum Colour { Red, Green } });
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains("this is an enum"), "{problems:?}");
    assert!(problems[0].contains("by hand"), "{problems:?}");
}

#[test]
fn a_union_is_refused_and_named_as_one() {
    let problems = refusal(quote! { union Raw { a: i64, b: f64 } });
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains("this is a union"), "{problems:?}");
}

#[test]
fn a_tuple_struct_is_refused_because_it_has_no_field_names() {
    let problems = refusal(quote! { struct Millis(i64); });
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(
        problems[0].contains("this is a tuple struct"),
        "{problems:?}"
    );
}

#[test]
fn a_unit_struct_is_refused() {
    let problems = refusal(quote! { struct Marker; });
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(
        problems[0].contains("this is a unit struct"),
        "{problems:?}"
    );
}

#[test]
fn a_struct_with_no_fields_is_accepted_as_an_empty_map() {
    // Distinct from a unit struct: `struct Empty {}` has a field list, it just
    // happens to be empty, and an empty `Value::Map` is a value the store holds
    // perfectly well.
    let model = accepted(quote! { struct Empty {} });
    assert!(model.fields.is_empty());
}

#[test]
fn every_flavour_of_generic_is_refused() {
    for generics in [
        quote!(<T>),
        quote!(<'a>),
        quote!(<const N: usize>),
        quote!(<T: Clone>),
    ] {
        let problems = refusal(quote! { struct Holder #generics { items: T } });
        assert_eq!(problems.len(), 1, "{problems:?} for {generics}");
        assert!(
            problems[0].contains("cannot describe a generic type"),
            "{problems:?} for {generics}"
        );
    }
}

#[test]
fn a_where_clause_alone_is_refused_too() {
    let problems = refusal(quote! { struct Holder where i64: Sized { count: i64 } });
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains("generic"), "{problems:?}");
}

#[test]
fn input_that_is_not_an_item_at_all_is_a_parse_error_not_a_panic() {
    let problems = refusal(quote! { let x = 1; });
    assert_eq!(problems.len(), 1, "{problems:?}");
}

#[test]
fn every_problem_in_one_struct_is_reported_at_once() {
    // Three distinct faults: a bad rename, a duplicate key, and an unknown
    // attribute. A developer should need one `cargo check`, not three.
    let problems = refusal(quote! {
        struct App {
            #[duet(rename = "a.b")]
            first: i64,
            #[duet(rename = "third")]
            second: i64,
            #[duet(nonsense)]
            third: i64,
        }
    });
    assert_eq!(problems.len(), 3, "{problems:?}");
    assert!(
        problems.iter().any(|p| p.contains("not a legal wire key")),
        "{problems:?}"
    );
    assert!(
        problems.iter().any(|p| p.contains("not a Duet attribute")),
        "{problems:?}"
    );
    assert!(
        problems.iter().any(|p| p.contains("share the wire key")),
        "{problems:?}"
    );
}
