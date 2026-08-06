//! Writing the impl out.
//!
//! # No logic in the generated code
//!
//! Everything below is a fixed shape with the model's names substituted into
//! it. There are no branches a reviewer has to simulate, no helper functions
//! hidden in a `const _: () = { ... }`, and no control flow beyond "this key was
//! absent" and "this field did not decode". That is the same reason
//! `SharedState::schema` returns a bare `Ty` rather than a `Result`: expanded
//! output should read as a list of field descriptions.
//!
//! # Hygiene
//!
//! Two rules, and `tests/hygiene.rs` checks both against a module that shadows
//! `Result`, `Option`, `Ok`, `Err`, `Some`, `None`, `String` and `Vec` and never
//! mentions `duet`:
//!
//! - Every path out of the generated code is absolute: `#krate::...` (which is
//!   `::duet` unless `#[duet(crate = ...)]` says otherwise), `::core::...`, and
//!   `::std::...`. A leading `::` resolves in the extern prelude, which no item
//!   in the user's crate can shadow.
//! - Every local binding is minted at [`Span::mixed_site`], so it is in a
//!   different syntax context from anything the user wrote and cannot capture
//!   or be captured by it.

use proc_macro2::{Span, TokenStream};
use quote::quote;

use crate::model::{Field, Model, Role};

/// The two impls one `#[derive(SharedState)]` produces.
pub fn impls(model: &Model) -> TokenStream {
    let shared_state = shared_state(model);
    let not_nullable = not_nullable(model);
    quote! {
        #shared_state
        #not_nullable
    }
}

/// `impl SharedState for #ident`.
fn shared_state(model: &Model) -> TokenStream {
    let Model { ident, krate, .. } = model;
    let to_value = to_value(model);
    let from_value = from_value(model);
    let schema = schema(model);
    quote! {
        #[automatically_derived]
        impl #krate::SharedState for #ident {
            #to_value
            #from_value
            #schema
        }
    }
}

/// `impl NotNullable for #ident`.
///
/// Unconditional, and sound for every struct this derive accepts: `to_value`
/// below always produces a [`Value::Map`](duet_core::Value::Map), so there is no
/// value of the type that lowers to `Value::Null` and no `Option<#ident>` that
/// could collapse `Some(_)` and `None` onto one wire value.
fn not_nullable(model: &Model) -> TokenStream {
    let Model { ident, krate, .. } = model;
    quote! {
        #[automatically_derived]
        impl #krate::NotNullable for #ident {}
    }
}

/// `fn to_value`: one map entry per shared field, in declaration order.
fn to_value(model: &Model) -> TokenStream {
    let krate = &model.krate;
    let entries: Vec<TokenStream> = model
        .fields
        .iter()
        .filter_map(|field| {
            let (key, ty, ident) = (field.key()?, &field.ty, &field.ident);
            Some(quote! {
                (#key, <#ty as #krate::SharedState>::to_value(&self.#ident))
            })
        })
        .collect();
    quote! {
        fn to_value(&self) -> #krate::Value {
            #krate::Value::map([#(#entries),*])
        }
    }
}

/// `fn from_value`: total over every [`Value`](duet_core::Value), as the trait
/// requires.
///
/// A guest can write anything to any path, so this answers for a `Value::Bytes`
/// where a struct was expected as calmly as for a well-formed map — and it never
/// panics, because there is nothing in it that can.
fn from_value(model: &Model) -> TokenStream {
    let Model {
        ident, krate, name, ..
    } = model;
    let value = hidden("value");
    let entries = hidden("entries");
    let inits: Vec<TokenStream> = model
        .fields
        .iter()
        .map(|field| field_init(model, field, &entries))
        .collect();
    let bind = bind_entries(model, &entries);
    quote! {
        fn from_value(
            #value: &#krate::Value,
        ) -> ::core::result::Result<Self, #krate::DecodeError> {
            let #entries = match #value {
                #krate::Value::Map(#entries) => #entries,
                _ => return ::core::result::Result::Err(
                    #krate::DecodeError::wrong_type(#name, #value),
                ),
            };
            #bind
            ::core::result::Result::Ok(#ident { #(#inits),* })
        }
    }
}

/// Silences `unused_variables` for a struct whose every field is skipped.
///
/// The map check above still has to run — a `Value::Str` is not that struct,
/// whatever the struct's fields are — so the binding exists and simply has no
/// reader.
fn bind_entries(model: &Model, entries: &syn::Ident) -> TokenStream {
    if model.fields.iter().any(|field| field.key().is_some()) {
        return TokenStream::new();
    }
    quote! { let _ = #entries; }
}

/// One field of the struct literal `from_value` returns.
fn field_init(model: &Model, field: &Field, entries: &syn::Ident) -> TokenStream {
    let Model { krate, name, .. } = model;
    let (ident, ty) = (&field.ident, &field.ty);
    let Role::Shared { key, .. } = &field.role else {
        return quote! {
            #ident: <#ty as #krate::SkippedDefault>::skipped_default()
        };
    };
    let (found, decoded, bad) = (hidden("found"), hidden("decoded"), hidden("bad"));
    quote! {
        #ident: {
            let #found = match #entries.get(#key) {
                ::core::option::Option::Some(#found) => #found,
                ::core::option::Option::None => return ::core::result::Result::Err(
                    #krate::DecodeError::missing_field(#name, #key),
                ),
            };
            match <#ty as #krate::SharedState>::from_value(#found) {
                ::core::result::Result::Ok(#decoded) => #decoded,
                ::core::result::Result::Err(#bad) => return ::core::result::Result::Err(
                    #krate::DecodeError::within_key(#bad, #key),
                ),
            }
        }
    }
}

/// `fn schema`: the definition this type registers, and the reference to it.
fn schema(model: &Model) -> TokenStream {
    let Model { krate, name, .. } = model;
    let (registry, inner) = (hidden("registry"), hidden("inner"));
    let described: Vec<TokenStream> = model
        .fields
        .iter()
        .filter_map(|field| {
            let (key, ty) = (field.key()?, &field.ty);
            Some(quote! {
                #krate::FieldDef::new(#key, <#ty as #krate::SharedState>::schema(#inner))
            })
        })
        .collect();
    let fields = if described.is_empty() {
        quote! { |_| ::std::vec::Vec::new() }
    } else {
        quote! { |#inner| ::std::vec::Vec::from([#(#described),*]) }
    };
    quote! {
        fn schema(#registry: &mut #krate::Registry) -> #krate::Ty {
            #krate::Registry::define::<Self>(#registry, #name, #fields)
        }
    }
}

/// A local binding the user's code can neither see nor shadow.
fn hidden(name: &str) -> syn::Ident {
    syn::Ident::new(name, Span::mixed_site())
}

#[cfg(test)]
#[path = "generate_tests.rs"]
mod tests;
