//! What the generated tokens contain.
//!
//! These assertions are about *shape*, not about behaviour: whether the derived
//! impls compile and agree with the schema is settled by `tests/`, where the
//! output is handed to the real compiler and the result is compared against the
//! committed `schema/app.json`. What is checked here is the handful of
//! decisions this module makes — which paths it writes, which fields it skips,
//! and the two empty cases that have no field to infer a type from.

use super::*;
use crate::model::Model;
use quote::quote;

/// The impls a definition generates.
fn generated(tokens: TokenStream) -> TokenStream {
    let model = Model::parse(tokens).unwrap_or_else(|e| panic!("should be accepted: {e}"));
    impls(&model)
}

/// True if `needle` appears in `haystack`.
///
/// Both sides are printed by `quote`, so the spacing matches by construction
/// and a needle can be written as the Rust it stands for. It must not *end*
/// part-way through a bracketed group, because a group is printed with both of
/// its delimiters.
fn holds(haystack: &TokenStream, needle: TokenStream) -> bool {
    haystack.to_string().contains(&needle.to_string())
}

/// An identifier, for interpolating a `&str` into a needle.
fn ident(name: &str) -> syn::Ident {
    syn::Ident::new(name, Span::call_site())
}

fn app() -> TokenStream {
    generated(quote! {
        struct App { counter: i64, editor: Editor, title: String }
    })
}

#[test]
fn both_impls_are_written_and_marked_as_derived() {
    let out = app();
    assert!(
        holds(&out, quote!(impl ::duet::SharedState for App)),
        "{out}"
    );
    assert!(
        holds(&out, quote!(impl ::duet::NotNullable for App)),
        "{out}"
    );
    assert_eq!(
        out.to_string().matches("automatically_derived").count(),
        2,
        "{out}"
    );
}

#[test]
fn every_field_goes_through_its_own_type_rather_than_through_inference() {
    // `<FieldTy as SharedState>::...` is what makes rejection the absence of an
    // impl: the bound lands on the field's *resolved* type, which no syntactic
    // special case could have got right.
    let out = app();
    for method in ["to_value", "from_value", "schema"] {
        let method = ident(method);
        assert!(
            holds(&out, quote!(<i64 as ::duet::SharedState>::#method)),
            "{method} does not go through the field's type: {out}"
        );
    }
}

#[test]
fn the_wire_key_and_the_type_name_reach_the_generated_strings() {
    let out = app();
    assert!(holds(&out, quote!(::duet::FieldDef::new)), "{out}");
    assert!(
        holds(&out, quote!(::duet::Registry::define::<Self>)),
        "{out}"
    );
    assert!(holds(&out, quote!(::duet::Value::map)), "{out}");
    assert!(out.to_string().contains("\"App\""), "{out}");
    assert!(out.to_string().contains("\"counter\""), "{out}");
}

#[test]
fn a_rename_reaches_every_place_the_key_is_written_and_the_field_name_none() {
    let out = generated(quote! {
        struct Window {
            #[duet(rename = "window_title")]
            title: String,
        }
    });
    let rendered = out.to_string();
    // Once in `to_value`, three times in `from_value` (the lookup, the absent
    // report, the location of a failed decode), once in `schema`.
    assert_eq!(rendered.matches("\"window_title\"").count(), 5, "{out}");
    // `title` survives only as the Rust field it is — `self.title` and the
    // struct literal's `title:`. Never as a key.
    assert!(!rendered.contains("\"title\""), "{out}");
}

#[test]
fn a_skipped_field_is_defaulted_and_appears_in_no_map_and_no_schema() {
    let out = generated(quote! {
        struct App {
            counter: i64,
            #[duet(skip)]
            cache: Blob,
        }
    });
    assert!(
        holds(
            &out,
            quote!(cache: <Blob as ::duet::SkippedDefault>::skipped_default())
        ),
        "{out}"
    );
    assert!(!out.to_string().contains("\"cache\""), "{out}");
}

#[test]
fn the_crate_attribute_redirects_every_path_including_the_skip_bound() {
    let out = generated(quote! {
        #[duet(crate = ::my_reexport)]
        struct App {
            counter: i64,
            #[duet(skip)]
            cache: Blob,
        }
    });
    assert!(!out.to_string().contains(":: duet ::"), "{out}");
    for item in [
        "SharedState",
        "NotNullable",
        "Value",
        "Registry",
        "Ty",
        "FieldDef",
        "DecodeError",
        "SkippedDefault",
    ] {
        let item = ident(item);
        assert!(holds(&out, quote!(::my_reexport::#item)), "{item}: {out}");
    }
}

#[test]
fn a_struct_with_no_shared_fields_still_refuses_a_value_that_is_not_a_map() {
    // Both empty cases at once: no map entries to infer an element type from,
    // no field descriptions, and a bound `entries` with no reader.
    let out = generated(quote! {
        struct Empty {
            #[duet(skip)]
            cache: Blob,
        }
    });
    assert!(
        holds(&out, quote!(::duet::DecodeError::wrong_type)),
        "{out}"
    );
    assert!(holds(&out, quote!(::std::vec::Vec::new())), "{out}");
    assert!(out.to_string().contains("let _ ="), "{out}");
}

#[test]
fn a_struct_with_shared_fields_does_not_silence_the_binding_it_uses() {
    assert!(!app().to_string().contains("let _ ="), "{}", app());
}

/// Every token in `tokens`, in order, descending into bracketed groups.
///
/// A `::` arrives as two separate `:` punctuation tokens, which is why the
/// check below looks two places back rather than one.
fn flattened(tokens: TokenStream, out: &mut Vec<String>) {
    for tree in tokens {
        match tree {
            proc_macro2::TokenTree::Group(group) => flattened(group.stream(), out),
            other => out.push(other.to_string()),
        }
    }
}

#[test]
fn every_prelude_name_the_output_mentions_is_written_out_in_full() {
    // The hygiene rule as a property of the tokens: each of these names appears
    // only as the tail of its absolute path, never on its own. `tests/hygiene.rs`
    // is the measurement — it compiles the output in a module that shadows all
    // eight — and this is the cheap check that fails first when a new
    // unqualified path is added.
    //
    // Compared token by token rather than by substring, because `Err` is a
    // substring of `DecodeError` and a substring count would answer about that
    // instead.
    let mut tokens = Vec::new();
    flattened(
        generated(quote! {
            struct App {
                counter: i64,
                #[duet(skip)]
                cache: Blob,
            }
        }),
        &mut tokens,
    );

    // Each shadowable name, and the path segment that must sit two `:` before
    // it. `String` has no entry: the output never mentions it at all.
    let parents = [
        ("Result", "result"),
        ("Ok", "Result"),
        ("Err", "Result"),
        ("Option", "option"),
        ("Some", "Option"),
        ("None", "Option"),
        ("Vec", "vec"),
    ];
    for (position, token) in tokens.iter().enumerate() {
        let Some((_, parent)) = parents.iter().find(|(name, _)| name == token) else {
            continue;
        };
        let qualified = position >= 3
            && tokens[position - 1] == ":"
            && tokens[position - 2] == ":"
            && tokens[position - 3] == *parent;
        assert!(
            qualified,
            "`{token}` is not written out in full: {tokens:?}"
        );
    }
    assert!(
        !tokens.iter().any(|token| token == "String"),
        "the output mentions `String`: {tokens:?}"
    );
}
