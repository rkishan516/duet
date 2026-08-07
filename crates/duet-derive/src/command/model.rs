//! What `#[command]` understood: a plain function, its argument keys, and
//! whether it asked for the context.
//!
//! Everything the macro is allowed to *decide* happens here. The generator does
//! no thinking — it walks this model and writes tokens — which is what keeps the
//! expanded output a list of argument descriptions a reviewer can check by
//! reading the diff.

use proc_macro2::{Span, TokenStream};

use crate::command::attr;
use crate::errors::Errors;
use crate::keys;
use crate::model::{duet_path, unraw};

/// A function `#[command]` accepted, ready to be written out.
pub struct Model {
    /// The function's identifier, which is also the hidden type's.
    pub ident: syn::Ident,
    /// The function's visibility, which the hidden type copies — a `pub`
    /// command has to be nameable in the `commands![…]` that registers it.
    pub vis: syn::Visibility,
    /// The name a guest invokes.
    pub name: String,
    /// The path the generated code names Duet by.
    pub krate: syn::Path,
    /// Every parameter, in declaration order — which is the order the schema
    /// records and therefore the order a generated client's arguments take.
    pub params: Vec<Param>,
    /// The declared return type; `()` when the function has no `->`.
    pub ret: syn::Type,
}

/// One parameter of the function.
pub enum Param {
    /// An argument, decoded out of the invocation's `args` under this key.
    Value {
        /// Its declared type, for `<#ty as CommandParam>::…`.
        ty: syn::Type,
        /// The `args` key it occupies, and one argument of every generated
        /// call.
        key: String,
        /// Where to point a complaint about that key: the `rename` literal
        /// when there was one, the parameter name when there was not.
        span: Span,
    },
    /// The invocation's context, produced rather than decoded.
    ///
    /// Recognised by **binding mode**: a parameter written as a reference is
    /// the context. What the reference points at is decided by trait
    /// resolution, not here — see `duet_command::FromContext`.
    Context {
        /// The reference type exactly as written, so the `FromContext` bound is
        /// checked against the developer's own spelling.
        ty: syn::Type,
    },
}

impl Model {
    /// Reads a `#[command]` input, or explains why it cannot.
    ///
    /// # Errors
    ///
    /// Every problem found, combined — a function with three bad parameters
    /// reports three, not the first.
    pub fn parse(attr: TokenStream, item: &syn::ItemFn) -> Result<Model, syn::Error> {
        let mut errors = Errors::new();
        let options = attr::Command::parse(attr, &mut errors);
        reject_unsupported_signature(&item.sig, &mut errors);

        let params = read_params(&item.sig, &mut errors);
        check_keys(&params, &mut errors);
        let name = command_name(item, options.rename.as_ref(), &mut errors);

        errors.finish(Model {
            ident: item.sig.ident.clone(),
            vis: item.vis.clone(),
            name,
            krate: options.krate.unwrap_or_else(duet_path),
            params,
            ret: return_type(&item.sig.output),
        })
    }
}

/// The name a guest invokes: the `rename`, or the function's own identifier.
///
/// Checked against `duet_schema::is_legal_command_name` — the real predicate the
/// schema validates with, not a re-derivation of it. `Schema::of_with_commands`
/// would refuse the same name at startup; refusing it here makes it a
/// `cargo check` error spanned on the literal that has to change.
fn command_name(item: &syn::ItemFn, rename: Option<&syn::LitStr>, errors: &mut Errors) -> String {
    let (name, span) = match rename {
        Some(literal) => (literal.value(), syn::spanned::Spanned::span(literal)),
        None => (unraw(&item.sig.ident), item.sig.ident.span()),
    };
    if !duet_schema::is_legal_command_name(&name) {
        errors.push(syn::Error::new(
            span,
            format!(
                "`{name}` is not a legal command name.\n\
                 A name is one or more identifiers separated by `.`, each starting with an ASCII \
                 letter and continuing with letters, digits or inner underscores — `subtract` and \
                 `documents.rename` are names, `2fast` and `documents.` are not. It has to be a \
                 legal identifier in Dart and TypeScript too, because a generated client turns it \
                 into a method.\n\
                 Fix: rename the function, or give it a legal name with \
                 `#[command(rename = \"...\")]`."
            ),
        ));
    }
    name
}

/// Refuses the signature shapes a command cannot have.
fn reject_unsupported_signature(sig: &syn::Signature, errors: &mut Errors) {
    if let Some(asyncness) = sig.asyncness {
        errors.at(
            asyncness,
            "`#[command]` cannot describe an `async fn`: a command body runs synchronously, on the \
             thread that called `dispatch_with`, and there is no async runtime anywhere in Duet to \
             hand a future to.\n\
             That is deliberate rather than unimplemented — on macOS that thread also drives the \
             UI, so a command that waits freezes the window it was called from.\n\
             Fixes: make the body synchronous, or spawn a thread of your own, return immediately, \
             and write the result into the store when it arrives — a subscription will deliver it.",
        );
    }
    if let Some(unsafety) = sig.unsafety {
        errors.at(
            unsafety,
            "`#[command]` cannot describe an `unsafe fn`: the generated body calls it, and a macro \
             cannot assert a safety contract on a caller's behalf.\n\
             Fix: wrap it in a safe function that upholds the contract, and put `#[command]` on \
             that.",
        );
    }
    if !sig.generics.params.is_empty() || sig.generics.where_clause.is_some() {
        errors.at(
            &sig.generics,
            "`#[command]` cannot describe a generic function: the schema names a command once, and \
             every instantiation of a generic would claim that one name with a different argument \
             shape.\n\
             Fix: write one `#[command]` per concrete instantiation you want to expose, each \
             delegating to the generic function.",
        );
    }
    if let Some(variadic) = &sig.variadic {
        errors.at(
            variadic,
            "`#[command]` cannot describe a variadic function: the schema records a fixed list of \
             named arguments, and there is no wire spelling for \"and then some more\".",
        );
    }
}

/// Reads every parameter, dropping the ones that could not be understood.
fn read_params(sig: &syn::Signature, errors: &mut Errors) -> Vec<Param> {
    sig.inputs
        .iter()
        .filter_map(|input| read_param(input, errors))
        .collect()
}

/// Reads one parameter and settles whether it is an argument or the context.
fn read_param(input: &syn::FnArg, errors: &mut Errors) -> Option<Param> {
    let typed = match input {
        syn::FnArg::Typed(typed) => typed,
        syn::FnArg::Receiver(receiver) => {
            errors.at(
                receiver,
                "`#[command]` cannot describe a method: a command is reached by name from a guest, \
                 with no receiver for it to be called on and nothing for the host to resolve one \
                 from.\n\
                 Fix: make it a free function that takes what it needs as arguments, and have it \
                 call the method.",
            );
            return None;
        }
    };
    let options = attr::Param::parse(&typed.attrs, errors);
    // A reference is the context slot. This is the one decision made from
    // syntax, and it has to be: a macro sees tokens, never resolved types, so
    // nothing else can tell a by-value binding from a by-reference one. *What*
    // the reference points at is left to `FromContext`, so both wrong spellings
    // fail closed at `cargo check`.
    if matches!(&*typed.ty, syn::Type::Reference(_)) {
        if let Some(rename) = &options.rename {
            errors.at(
                rename,
                "`#[duet(rename = \"...\")]` renames an argument key, and the context is not an \
                 argument: it is produced from the invocation rather than decoded out of `args`, \
                 so it occupies no key to rename.\n\
                 Fix: delete the `rename`.",
            );
        }
        return Some(Param::Context {
            ty: (*typed.ty).clone(),
        });
    }
    let ident = binding_name(&typed.pat, errors)?;
    let key = options
        .rename
        .as_ref()
        .map_or_else(|| unraw(&ident), syn::LitStr::value);
    Some(Param::Value {
        ty: (*typed.ty).clone(),
        key,
        span: attr::key_span(options.rename.as_ref(), &ident),
    })
}

/// The identifier a parameter binds, or the reason it does not have one.
///
/// An argument key is a parameter *name*, so anything that is not a plain name
/// has none. `_` is refused with the rest: a command's argument key is part of
/// its published contract, and there is nothing to name it after.
fn binding_name(pat: &syn::Pat, errors: &mut Errors) -> Option<syn::Ident> {
    match pat {
        syn::Pat::Ident(binding)
            if binding.by_ref.is_none() && binding.subpat.is_none() && binding.attrs.is_empty() =>
        {
            Some(binding.ident.clone())
        }
        other => {
            errors.at(
                other,
                "`#[command]` needs a plain name for every argument: the name is the key the \
                 argument occupies in the invocation's `args`, and it is what a generated Dart or \
                 TypeScript client calls the parameter.\n\
                 A pattern, a `ref` binding or a `_` has no name to use.\n\
                 Fixes: bind a name and destructure in the body, or take the whole thing as one \
                 argument of a `#[derive(SharedState)]` struct.",
            );
            None
        }
    }
}

/// Runs the shared key checks over the value parameters.
///
/// The very same [`keys::check_all`] `#[derive(SharedState)]`'s fields go
/// through: an argument occupies a key in a `Value::Map` exactly as a field
/// does, and is named in a generated client exactly as a field is.
fn check_keys(params: &[Param], errors: &mut Errors) {
    let keys: Vec<(&str, Span)> = params
        .iter()
        .filter_map(|param| match param {
            Param::Value { key, span, .. } => Some((key.as_str(), *span)),
            Param::Context { .. } => None,
        })
        .collect();
    keys::check_all(&keys, keys::Subject::Argument, errors);
}

/// The declared return type, with `()` standing in for no `->` at all.
///
/// Built by hand rather than through `parse_quote!`, which panics on a parse
/// failure — a proc macro must never panic, and "it cannot fail here" is an
/// argument, not a guarantee.
fn return_type(output: &syn::ReturnType) -> syn::Type {
    match output {
        syn::ReturnType::Type(_, ty) => (**ty).clone(),
        syn::ReturnType::Default => syn::Type::Tuple(syn::TypeTuple {
            paren_token: syn::token::Paren::default(),
            elems: syn::punctuated::Punctuated::new(),
        }),
    }
}

/// The function with every `#[duet(...)]` stripped from its parameters.
///
/// `#[command]` consumes the whole item and re-emits it, so the inert
/// `#[duet(...)]` markers have to come off on the way out: nothing downstream
/// registers that attribute, and leaving one on would turn a working command
/// into "cannot find attribute `duet`".
pub fn stripped(item: &syn::ItemFn) -> syn::ItemFn {
    let mut item = item.clone();
    for input in &mut item.sig.inputs {
        if let syn::FnArg::Typed(typed) = input {
            typed
                .attrs
                .retain(|attr| !attr.path().is_ident(crate::attr::NAMESPACE));
        }
    }
    item
}

#[cfg(test)]
#[path = "model_tests.rs"]
mod tests;
