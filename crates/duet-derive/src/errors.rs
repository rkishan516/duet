//! Collecting every problem in one derive, instead of the first.
//!
//! `duet-schema` reports **every** distinct problem a schema has rather than
//! stopping at the first, because a developer fixing type definitions benefits
//! from seeing them all at once. The derive owes the same courtesy: a struct
//! with three bad `rename`s should not need three `cargo check` runs.

use quote::ToTokens;

/// Zero or more [`syn::Error`]s, combined.
///
/// [`syn::Error`] is already a list — [`combine`](syn::Error::combine) appends
/// to it and [`to_compile_error`](syn::Error::to_compile_error) emits one
/// `compile_error!` per entry, each with its own span. This wrapper exists only
/// so the *empty* case is representable, which `syn::Error` itself has no way
/// to spell.
#[derive(Default)]
pub struct Errors(Option<syn::Error>);

impl Errors {
    /// An empty list.
    pub fn new() -> Errors {
        Errors(None)
    }

    /// Records one problem.
    pub fn push(&mut self, error: syn::Error) {
        match &mut self.0 {
            Some(existing) => existing.combine(error),
            None => self.0 = Some(error),
        }
    }

    /// Records one problem, spanned on `tokens`.
    pub fn at(&mut self, tokens: impl ToTokens, message: impl std::fmt::Display) {
        self.push(syn::Error::new_spanned(tokens, message));
    }

    /// `value` if nothing was recorded, otherwise everything that was.
    ///
    /// # Errors
    ///
    /// The combined [`syn::Error`], whenever [`push`](Errors::push) was called.
    pub fn finish<T>(self, value: T) -> Result<T, syn::Error> {
        match self.0 {
            Some(errors) => Err(errors),
            None => Ok(value),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proc_macro2::Span;

    /// Everything `errors` recorded, one message per problem.
    fn messages(errors: Errors) -> Vec<String> {
        match errors.finish(()) {
            Ok(()) => Vec::new(),
            Err(combined) => combined.into_iter().map(|e| e.to_string()).collect(),
        }
    }

    #[test]
    fn an_empty_list_yields_the_value_it_was_given() {
        assert_eq!(Errors::new().finish(7).ok(), Some(7));
        assert!(messages(Errors::new()).is_empty());
    }

    #[test]
    fn every_pushed_problem_survives_to_the_end_in_order() {
        let mut errors = Errors::new();
        errors.push(syn::Error::new(Span::call_site(), "first"));
        errors.push(syn::Error::new(Span::call_site(), "second"));
        assert_eq!(messages(errors), ["first", "second"]);
    }

    #[test]
    fn each_problem_keeps_its_own_span_and_becomes_its_own_compile_error() {
        let mut errors = Errors::new();
        errors.at(quote::quote!(counter), "first");
        errors.at(quote::quote!(title), "second");
        let combined = errors.finish(()).expect_err("two problems were recorded");
        let rendered = combined.to_compile_error().to_string();
        assert_eq!(rendered.matches("compile_error").count(), 2, "{rendered}");
    }
}
