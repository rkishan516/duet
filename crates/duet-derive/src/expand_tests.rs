//! The macro as a whole: tokens in, tokens out, never a panic.

use super::*;
use quote::quote;

#[test]
fn an_accepted_struct_expands_to_two_impls_and_no_compile_error() {
    let out = expand(quote! { struct App { counter: i64 } }).to_string();
    assert!(out.contains("impl :: duet :: SharedState for App"), "{out}");
    assert!(out.contains("impl :: duet :: NotNullable for App"), "{out}");
    assert!(!out.contains("compile_error"), "{out}");
}

#[test]
fn a_refused_struct_expands_to_compile_error_and_no_impl() {
    // A partial impl alongside the error would produce a second, spurious
    // failure — "not all trait items implemented" — competing with the one that
    // says what is actually wrong.
    let out = expand(quote! { enum Colour { Red } }).to_string();
    assert!(out.starts_with(":: core :: compile_error !"), "{out}");
    // `impl` appears inside the message's prose, so the check that no impl was
    // *emitted* is the marker every generated impl carries.
    assert!(!out.contains("automatically_derived"), "{out}");
}

#[test]
fn one_compile_error_is_emitted_per_problem() {
    let out = expand(quote! {
        struct App {
            #[duet(rename = "a.b")]
            first: i64,
            #[duet(nonsense)]
            second: i64,
        }
    })
    .to_string();
    assert_eq!(out.matches("compile_error").count(), 2, "{out}");
}

#[test]
fn nothing_a_caller_can_write_makes_the_macro_panic() {
    // A panicking proc macro reports "custom attribute panicked" with the
    // macro's own backtrace and no span into the user's file — the worst
    // diagnostic in the language to be handed for a misspelled attribute. Every
    // input below is refused, and refused as tokens.
    let hostile = [
        quote! {},
        quote! { let x = 1; },
        quote! { struct },
        quote! { fn nope() {} },
        quote! { struct App { #[duet(crate = 7)] counter: i64 } },
        quote! { struct App { #[duet(rename = 7)] counter: i64 } },
        quote! { struct App { #[duet(skip = "yes")] counter: i64 } },
        quote! { #[duet(crate)] struct App { counter: i64 } },
        quote! { struct App<T> { counter: T } },
        quote! { union U { a: i64 } },
    ];
    for input in hostile {
        let out = expand(input.clone()).to_string();
        assert!(
            out.contains("compile_error"),
            "{input} expanded to {out} instead of a refusal"
        );
    }
}
