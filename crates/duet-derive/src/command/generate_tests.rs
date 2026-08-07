//! Tests for the tokens `#[command]` writes.
//!
//! # What these do NOT catch, and where that is covered instead
//!
//! Everything here compares the generator's output to text written by reading
//! the generator. That proves the expansion is **stable** — a refactor that
//! changed it fails here — and proves nothing about whether it is **correct**.
//! A generator that decoded the second argument under the first argument's key
//! would produce a golden matching itself forever, and so would one whose
//! `describe` disagreed with its `run`.
//!
//! Three specific gaps, and what closes each:
//!
//! | Not caught here | Caught by |
//! |---|---|
//! | an argument bound under the wrong key | `tests/commands.rs`, over a live host, with a non-commutative body |
//! | `describe` and `run` disagreeing | `tests/commands.rs`, which asserts the schema and the replies from one set of functions |
//! | a rejected type slipping through | `tests/ui/`, where the compiler answers rather than a string comparison |
//!
//! `tests/mutation.rs` measures that split rather than asserting it: it breaks
//! one thing at a time and records which check notices.

use super::*;
use crate::command::model::Model;
use quote::quote;

/// The tokens `#[command]` produces for `item`, as one string.
fn expanded(item: TokenStream) -> String {
    let parsed: syn::ItemFn = syn::parse2(item).expect("the test item should parse");
    let model =
        Model::parse(TokenStream::new(), &parsed).expect("the test item should be accepted");
    impls(&model).to_string()
}

#[test]
fn the_marker_type_is_braced_so_it_does_not_collide_with_the_function() {
    // A unit struct would occupy the value namespace too and collide with the
    // function it is named after. A braced one exists only as a type, so `add`
    // is the function in expression position and the command in type position.
    let expanded = expanded(quote! { fn add(a: i64) -> i64 { a } });
    assert!(expanded.contains("struct add { }"), "{expanded}");
    assert!(expanded.contains("# [doc (hidden)]"), "{expanded}");
    assert!(expanded.contains("non_camel_case_types"), "{expanded}");
}

#[test]
fn the_marker_type_copies_the_functions_visibility() {
    // A `pub` command has to be nameable in the `commands![…]` that registers
    // it, which may be in another module.
    let parsed: syn::ItemFn =
        syn::parse2(quote! { pub fn add(a: i64) -> i64 { a } }).expect("parses");
    let model = Model::parse(TokenStream::new(), &parsed).expect("accepted");
    assert!(impls(&model).to_string().contains("pub struct add"));
}

#[test]
fn every_path_out_of_the_generated_code_is_absolute() {
    // A leading `::` resolves in the extern prelude, which no item in the
    // user's crate can shadow. `tests/hygiene.rs` measures this against a
    // module that shadows everything; this pins it at the token level so a
    // relative path is caught before it reaches a compiler.
    let expanded = expanded(quote! { fn f(a: i64, ctx: &CommandContext) -> i64 { a } });
    for absolute in [
        ":: duet :: Command",
        ":: duet :: CommandDef",
        ":: duet :: CommandParam",
        ":: duet :: FromContext",
        ":: duet :: command_returns",
        ":: duet :: command_raises",
        ":: duet :: into_outcome",
        ":: duet :: Outcome :: Refused",
        ":: core :: result :: Result",
        ":: std :: string :: String",
        ":: std :: vec :: Vec",
    ] {
        assert!(
            expanded.contains(absolute),
            "{absolute} missing: {expanded}"
        );
    }
}

#[test]
fn describe_lists_the_arguments_and_asks_the_return_type_for_both_reply_types() {
    let expanded = expanded(quote! { fn subtract(a: i64, b: i64) -> i64 { a - b } });
    assert!(
        expanded.contains(
            ":: duet :: FieldDef :: new (\"a\" , < i64 as :: duet :: CommandParam > :: param_ty"
        ),
        "{expanded}"
    );
    assert!(
        expanded.contains(
            ":: duet :: FieldDef :: new (\"b\" , < i64 as :: duet :: CommandParam > :: param_ty"
        ),
        "{expanded}"
    );
    assert!(
        expanded.contains("returns : :: duet :: command_returns :: < i64 , _ >"),
        "{expanded}"
    );
    assert!(
        expanded.contains("raises : :: duet :: command_raises :: < i64 , _ >"),
        "{expanded}"
    );
}

#[test]
fn the_context_reaches_describe_as_nothing_at_all() {
    // It is not an argument, so it occupies no key and appears in no schema.
    let expanded = expanded(quote! { fn f(ctx: &CommandContext, by: i64) {} });
    assert_eq!(expanded.matches("FieldDef :: new").count(), 1, "{expanded}");
    assert!(expanded.contains("\"by\""), "{expanded}");
    assert!(!expanded.contains("\"ctx\""), "{expanded}");
}

#[test]
fn run_binds_every_parameter_in_declaration_order_and_calls_the_function() {
    let expanded = expanded(quote! { fn f(a: i64, ctx: &CommandContext, b: i64) {} });
    let call = expanded
        .split("into_outcome (")
        .nth(1)
        .expect("the call is generated");
    assert!(
        call.starts_with("f (argument0 , argument1 , argument2)"),
        "{call}"
    );
    let (first, second) = (
        expanded.find("argument0 = match").expect("a is decoded"),
        expanded.find("argument2 = match").expect("b is decoded"),
    );
    assert!(first < second, "declaration order: {expanded}");
    assert!(
        expanded.contains("argument1 = < & CommandContext as :: duet :: FromContext >"),
        "{expanded}"
    );
}

#[test]
fn a_failed_argument_decode_returns_a_refusal_rather_than_raising() {
    // The call never got as far as doing anything, which is a `failed` and not
    // a `raised`. A generated body that raised instead would tell a guest that
    // something ran and failed.
    let expanded = expanded(quote! { fn f(a: i64) {} });
    assert!(
        expanded.contains("return :: duet :: Outcome :: Refused (why) ;"),
        "{expanded}"
    );
    assert!(!expanded.contains("Outcome :: Raised"), "{expanded}");
}

#[test]
fn an_unread_parameter_of_run_is_underscored_rather_than_discarded() {
    // Underscoring keeps the expansion free of statements that exist only to
    // silence a lint.
    let neither = expanded(quote! { fn f() {} });
    assert!(neither.contains("_args :"), "{neither}");
    assert!(neither.contains("_context :"), "{neither}");

    let both = expanded(quote! { fn f(a: i64, ctx: &CommandContext) {} });
    assert!(both.contains("args :"), "{both}");
    assert!(!both.contains("_args :"), "{both}");
    assert!(both.contains("context :"), "{both}");
    assert!(!both.contains("_context :"), "{both}");
}

#[test]
fn no_return_type_asks_the_unit_type_for_its_description() {
    let expanded = expanded(quote! { fn f() {} });
    assert!(
        expanded.contains("command_returns :: < () , _ >"),
        "{expanded}"
    );
}

#[test]
fn a_crate_attribute_redirects_every_path_in_the_expansion() {
    let parsed: syn::ItemFn = syn::parse2(quote! { fn f(a: i64) {} }).expect("parses");
    let model = Model::parse(quote! { crate = ::my_reexport }, &parsed).expect("accepted");
    let expanded = impls(&model).to_string();
    assert!(!expanded.contains(":: duet ::"), "{expanded}");
    assert!(expanded.contains(":: my_reexport :: Command"), "{expanded}");
}

#[test]
fn the_expansion_is_byte_stable_across_repeated_runs() {
    // The generator must be a function of the model. Anything drawn from a hash
    // map's iteration order would show up here.
    let item = quote! { fn f(a: i64, ctx: &CommandContext, b: String) -> Result<i64, E> { }};
    assert_eq!(expanded(item.clone()), expanded(item));
}

#[test]
fn nothing_generated_names_the_users_own_bindings() {
    // Every local is minted at `Span::mixed_site`, so a user's `args` or `why`
    // can neither capture nor be captured by the expansion. The names still
    // appear as text, which is why `tests/hygiene.rs` compiles the real thing
    // against a module that shadows everything.
    let expanded = expanded(quote! { fn f(a: i64) -> i64 { a } });
    assert!(expanded.contains("argument0"), "{expanded}");
    assert_eq!(
        expanded.matches("let a =").count(),
        0,
        "a user's parameter name must not become a generated binding: {expanded}"
    );
}

#[test]
fn the_name_the_guest_invokes_is_the_one_the_model_settled() {
    let parsed: syn::ItemFn = syn::parse2(quote! { fn add() {} }).expect("parses");
    let model = Model::parse(quote! { rename = "documents.add" }, &parsed).expect("accepted");
    let expanded = impls(&model).to_string();
    assert!(
        expanded.contains("const NAME : & 'static str = \"documents.add\""),
        "{expanded}"
    );
    assert!(
        expanded.contains("String :: from (\"documents.add\")"),
        "{expanded}"
    );
    assert!(
        expanded.contains("struct add { }"),
        "the type keeps the Rust name: {expanded}"
    );
}
