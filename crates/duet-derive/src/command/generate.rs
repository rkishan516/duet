//! Writing the `Command` impl out.
//!
//! # No logic in the generated code
//!
//! Everything below is a fixed shape with the model's names substituted into
//! it. There is one `match` per argument, and it is the same three lines every
//! time; the wording of a refusal lives in `duet_command::CommandParam` rather
//! than in the expansion, so a reviewer reading a diff sees a list of arguments
//! and not a decoder.
//!
//! # Hygiene
//!
//! Two rules, and `tests/hygiene.rs` checks both against a module that shadows
//! `Result`, `Option`, `Ok`, `Err`, `Some`, `None`, `String` and `Vec` and never
//! mentions `duet`:
//!
//! - Every path out of the generated code is absolute: `#krate::...` (which is
//!   `::duet` unless `#[command(crate = ...)]` says otherwise), `::core::...`
//!   and `::std::...`.
//! - Every local binding is minted at [`Span::mixed_site`], so it is in a
//!   different syntax context from anything the user wrote and cannot capture
//!   or be captured by it.

use proc_macro2::{Span, TokenStream};
use quote::quote;

use crate::command::model::{Model, Param};

/// The hidden type and the `Command` impl one `#[command]` produces.
pub fn impls(model: &Model) -> TokenStream {
    let marker = marker_type(model);
    let command = command_impl(model);
    quote! {
        #marker
        #command
    }
}

/// `struct #ident {}` — the type `commands![…]` names.
///
/// A **braced** struct, deliberately. A unit struct would occupy the value
/// namespace too and collide with the function it is named after; a braced one
/// exists only as a type, so `add` is the function in expression position and
/// the command in type position, and one `use` brings both.
fn marker_type(model: &Model) -> TokenStream {
    let Model { ident, vis, .. } = model;
    quote! {
        #[doc(hidden)]
        #[automatically_derived]
        #[allow(non_camel_case_types)]
        #vis struct #ident {}
    }
}

/// `impl Command for #ident`.
fn command_impl(model: &Model) -> TokenStream {
    let Model {
        ident, krate, name, ..
    } = model;
    let describe = describe(model);
    let run = run(model);
    quote! {
        #[automatically_derived]
        impl #krate::Command for #ident {
            const NAME: &'static str = #name;
            #describe
            #run
        }
    }
}

/// `fn describe`: one `FieldDef` per argument, then the two reply types.
fn describe(model: &Model) -> TokenStream {
    let Model { krate, name, .. } = model;
    let registry = hidden("registry");
    let ret = &model.ret;
    let params: Vec<TokenStream> = model
        .params
        .iter()
        .filter_map(|param| {
            let Param::Value { ty, key, .. } = param else {
                return None;
            };
            Some(quote! {
                #krate::FieldDef::new(
                    #key,
                    <#ty as #krate::CommandParam>::param_ty(#registry),
                )
            })
        })
        .collect();
    quote! {
        fn describe(#registry: &mut #krate::Registry) -> #krate::CommandDef {
            #krate::CommandDef {
                name: ::std::string::String::from(#name),
                params: ::std::vec::Vec::from([#(#params),*]),
                returns: #krate::command_returns::<#ret, _>(#registry),
                raises: #krate::command_raises::<#ret, _>(#registry),
            }
        }
    }
}

/// `fn run`: bind every parameter, call the function, lower what it returned.
fn run(model: &Model) -> TokenStream {
    let Model { ident, krate, .. } = model;
    let args = binding_for("args", model.params.iter().any(is_value));
    let context = binding_for("context", model.params.iter().any(is_context));
    let bindings: Vec<TokenStream> = model
        .params
        .iter()
        .enumerate()
        .map(|(index, param)| bind(model, param, index, &args, &context))
        .collect();
    let call: Vec<syn::Ident> = (0..model.params.len()).map(argument).collect();
    quote! {
        fn run(
            #args: #krate::Args,
            #context: &#krate::CommandContext,
        ) -> #krate::Outcome {
            #(#bindings)*
            // A command with no return type calls a function returning `()` and
            // hands that `()` on, which is exactly what this lint is about and
            // exactly what is wanted here.
            #[allow(clippy::unit_arg)]
            #krate::into_outcome(#ident(#(#call),*))
        }
    }
}

/// One parameter's binding: decoded from `args`, or produced from the context.
fn bind(
    model: &Model,
    param: &Param,
    index: usize,
    args: &syn::Ident,
    context: &syn::Ident,
) -> TokenStream {
    let krate = &model.krate;
    let binding = argument(index);
    match param {
        Param::Context { ty } => quote! {
            let #binding = <#ty as #krate::FromContext>::from_context(#context);
        },
        Param::Value { ty, key, .. } => {
            let why = hidden("why");
            quote! {
                let #binding = match <#ty as #krate::CommandParam>::from_args(#key, &#args) {
                    ::core::result::Result::Ok(#binding) => #binding,
                    // An argument that is missing or is not this type means the
                    // call never got as far as doing anything, which is a
                    // `failed` and not a `raised`.
                    ::core::result::Result::Err(#why) => {
                        return #krate::Outcome::Refused(#why);
                    }
                };
            }
        }
    }
}

/// True if this parameter is decoded out of `args`.
fn is_value(param: &Param) -> bool {
    matches!(param, Param::Value { .. })
}

/// True if this parameter is the invocation's context.
fn is_context(param: &Param) -> bool {
    matches!(param, Param::Context { .. })
}

/// The binding for parameter `index`.
fn argument(index: usize) -> syn::Ident {
    hidden(&format!("argument{index}"))
}

/// A `run` parameter's name, underscored when nothing reads it.
///
/// A command with no arguments does not read `args`, and one that does not ask
/// for the context does not read it either. Underscoring rather than emitting a
/// `let _ = …` keeps the expansion free of statements that exist only to
/// silence a lint.
fn binding_for(name: &str, used: bool) -> syn::Ident {
    if used {
        hidden(name)
    } else {
        hidden(&format!("_{name}"))
    }
}

/// A local binding the user's code can neither see nor shadow.
fn hidden(name: &str) -> syn::Ident {
    syn::Ident::new(name, Span::mixed_site())
}

#[cfg(test)]
#[path = "generate_tests.rs"]
mod tests;
