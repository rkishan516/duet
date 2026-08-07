//! `#[command]`: one Rust function, one host command a guest may invoke.
//!
//! Behind the `commands` Cargo feature, which is what turns on `syn`'s `full`
//! feature — this macro has to parse a whole function item, and
//! `#[derive(SharedState)]` does not, so a crate that only derives should not
//! pay for the parser.

mod attr;
mod generate;
mod model;

use proc_macro2::TokenStream;
use quote::quote;

/// The whole macro, on `proc_macro2` tokens so it is callable from a test.
///
/// A refusal expands to `compile_error!` invocations — one per problem, each
/// spanned on the token that caused it — and **never** to a panic. A panicking
/// proc macro reports "custom attribute panicked" with the macro's own
/// backtrace and no span into the user's file, which is the worst diagnostic in
/// the language to be handed for a misspelled attribute.
///
/// # The function is re-emitted whatever happens
///
/// Even when the model is refused, and even when the item did not parse as a
/// function at all. A `#[command]` that erased the item it was written on would
/// turn one mistake into a page of "cannot find function" errors at every call
/// site, and the real complaint would be the one nobody scrolled to.
pub fn expand(attr: TokenStream, item: TokenStream) -> TokenStream {
    let parsed: syn::ItemFn = match syn::parse2(item.clone()) {
        Ok(parsed) => parsed,
        Err(error) => {
            let error = not_a_function(&error).to_compile_error();
            return quote! { #item #error };
        }
    };
    let stripped = model::stripped(&parsed);
    match model::Model::parse(attr, &parsed) {
        Ok(model) => {
            let impls = generate::impls(&model);
            quote! { #stripped #impls }
        }
        Err(errors) => {
            let errors = errors.to_compile_error();
            quote! { #stripped #errors }
        }
    }
}

/// Re-words `syn`'s parse failure so it names what `#[command]` wanted.
///
/// `syn`'s own message is about the token it stopped on — "expected `fn`" — and
/// says nothing about why anything expected a `fn` in the first place. The
/// original span is kept, because it is still the right place to point.
fn not_a_function(error: &syn::Error) -> syn::Error {
    syn::Error::new(
        error.span(),
        format!(
            "`#[command]` describes a function, and this is not one ({error}).\n\
             A command is a plain Rust function: `#[command] fn add(a: i64, b: i64) -> i64`. It \
             stays callable from Rust exactly as it was — the macro adds a description of it \
             beside it rather than replacing it.\n\
             Fix: move `#[command]` onto the function you meant."
        ),
    )
}

#[cfg(test)]
#[path = "command_tests.rs"]
mod tests;
