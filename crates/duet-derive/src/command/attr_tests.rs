//! Tests for the two `#[command(...)]` keys and the one `#[duet(...)]` key.

use super::*;
use quote::quote;

/// Reads `#[command(...)]` arguments, yielding the options and any complaints.
fn command(attr: TokenStream) -> (Command, Vec<String>) {
    let mut errors = Errors::new();
    let found = Command::parse(attr, &mut errors);
    (found, messages(errors))
}

/// Reads `#[duet(...)]` on a parameter.
fn param(attrs: TokenStream) -> (Param, Vec<String>) {
    let parsed: syn::ItemFn =
        syn::parse2(quote! { fn f(#attrs a: i64) {} }).expect("the test signature should parse");
    let syn::FnArg::Typed(typed) = &parsed.sig.inputs[0] else {
        panic!("the test signature has a typed parameter");
    };
    let mut errors = Errors::new();
    let found = Param::parse(&typed.attrs, &mut errors);
    (found, messages(errors))
}

/// Everything `errors` recorded, one message per problem.
fn messages(errors: Errors) -> Vec<String> {
    match errors.finish(()) {
        Ok(()) => Vec::new(),
        Err(combined) => combined.into_iter().map(|e| e.to_string()).collect(),
    }
}

#[test]
fn no_arguments_at_all_is_the_ordinary_case() {
    let (found, problems) = command(TokenStream::new());
    assert!(found.rename.is_none());
    assert!(found.krate.is_none());
    assert!(problems.is_empty(), "{problems:?}");
}

#[test]
fn both_keys_are_read() {
    let (found, problems) = command(quote! { rename = "documents.add", crate = ::my_reexport });
    assert_eq!(
        found.rename.map(|r| r.value()),
        Some("documents.add".into())
    );
    assert!(found.krate.is_some());
    assert!(problems.is_empty(), "{problems:?}");
}

#[test]
fn a_key_given_twice_is_refused_rather_than_merged() {
    // Two renames are a developer who edited one and forgot the other; silently
    // keeping either is how the wrong name ships.
    for attr in [
        quote! { rename = "a", rename = "b" },
        quote! { crate = ::a, crate = ::b },
    ] {
        let (_, problems) = command(attr.clone());
        assert_eq!(problems.len(), 1, "{attr}: {problems:?}");
        assert!(problems[0].contains("given twice"), "{problems:?}");
    }
}

#[test]
fn skip_is_refused_with_a_message_saying_why_it_cannot_mean_anything() {
    let (_, problems) = command(quote! { skip });
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(
        problems[0].contains("no meaning on a command"),
        "{problems:?}"
    );
}

#[test]
fn an_unrecognised_key_is_refused_and_the_two_real_ones_are_named() {
    // A typo in `#[command(renmae = "...")]` that compiled would ship the wrong
    // command name.
    let (_, problems) = command(quote! { renmae = "add" });
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains("renmae"), "{problems:?}");
    assert!(problems[0].contains("rename"), "{problems:?}");
    assert!(problems[0].contains("crate"), "{problems:?}");
}

#[test]
fn a_rejected_key_does_not_also_produce_a_syntax_complaint() {
    // Without the swallow, the nested-meta loop goes on to demand a `,` where
    // the rejected key's `= 1` still sits: two complaints for one edit, one of
    // them noise.
    let (_, problems) = command(quote! { renmae = "add", rename = "ok" });
    assert_eq!(problems.len(), 1, "{problems:?}");
}

#[test]
fn a_parameter_reads_its_rename() {
    let (found, problems) = param(quote! { #[duet(rename = "window_title")] });
    assert_eq!(found.rename.map(|r| r.value()), Some("window_title".into()));
    assert!(problems.is_empty(), "{problems:?}");
}

#[test]
fn a_parameter_with_no_attribute_has_no_rename() {
    let (found, problems) = param(TokenStream::new());
    assert!(found.rename.is_none());
    assert!(problems.is_empty(), "{problems:?}");
}

#[test]
fn crate_on_a_parameter_is_refused_and_pointed_at_the_function() {
    let (_, problems) = param(quote! { #[duet(crate = ::duet)] });
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(
        problems[0].contains("belongs on the function"),
        "{problems:?}"
    );
}

#[test]
fn an_unrecognised_parameter_key_is_refused() {
    for attrs in [quote! { #[duet(skip)] }, quote! { #[duet(renmae = "a")] }] {
        let (_, problems) = param(attrs.clone());
        assert_eq!(problems.len(), 1, "{attrs}: {problems:?}");
        assert!(
            problems[0].contains("not a Duet parameter attribute"),
            "{problems:?}"
        );
    }
}

#[test]
fn a_bare_duet_attribute_says_what_to_write_instead() {
    let (_, problems) = param(quote! { #[duet] });
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(
        problems[0].contains("says nothing on its own"),
        "{problems:?}"
    );
}

#[test]
fn a_parameter_rename_given_twice_is_refused() {
    let (_, problems) = param(quote! { #[duet(rename = "a")] #[duet(rename = "b")] });
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains("given twice"), "{problems:?}");
}

#[test]
fn a_rename_that_is_not_a_string_is_refused_without_panicking() {
    for attrs in [quote! { #[duet(rename = 7)] }, quote! { #[duet(rename)] }] {
        let (found, problems) = param(attrs.clone());
        assert!(found.rename.is_none(), "{attrs}");
        assert_eq!(problems.len(), 1, "{attrs}: {problems:?}");
    }
}
