//! Tests for [`expand`](super::expand): what survives a refusal, and that
//! nothing panics.

use super::*;

/// Every `compile_error!` message `expand` produced, or none.
fn refusals(attr: TokenStream, item: TokenStream) -> Vec<String> {
    let expanded = expand(attr, item).to_string();
    expanded
        .match_indices("compile_error !")
        .filter_map(|(at, _)| {
            let rest = &expanded[at..];
            let open = rest.find('"')?;
            let close = rest[open + 1..].find('"')?;
            Some(rest[open + 1..open + 1 + close].to_string())
        })
        .collect()
}

/// `#[command] fn add(a: i64, b: i64) -> i64`, with no attribute arguments.
fn add() -> TokenStream {
    quote! { fn add(a: i64, b: i64) -> i64 { a - b } }
}

#[test]
fn an_accepted_function_expands_to_itself_plus_an_impl() {
    let expanded = expand(TokenStream::new(), add()).to_string();
    assert!(expanded.contains("fn add"), "{expanded}");
    assert!(
        expanded.contains("impl :: duet :: Command for add"),
        "{expanded}"
    );
    assert!(!expanded.contains("compile_error"), "{expanded}");
}

#[test]
fn a_refused_function_is_still_emitted_alongside_its_complaint() {
    // A `#[command]` that erased the item it was written on would turn one
    // mistake into a page of "cannot find function" errors at every call site,
    // and the real complaint would be the one nobody scrolled to.
    let expanded = expand(TokenStream::new(), quote! { async fn add() {} }).to_string();
    assert!(expanded.contains("async fn add"), "{expanded}");
    assert!(expanded.contains("compile_error"), "{expanded}");
}

#[test]
fn an_item_that_is_not_a_function_is_refused_with_a_message_naming_what_was_wanted() {
    let refusals = refusals(TokenStream::new(), quote! { struct Add { a: i64 } });
    assert_eq!(refusals.len(), 1, "{refusals:?}");
    assert!(refusals[0].contains("describes a function"), "{refusals:?}");
}

#[test]
fn a_non_function_item_is_still_emitted_so_its_users_do_not_all_break() {
    let expanded = expand(TokenStream::new(), quote! { struct Add { a: i64 } }).to_string();
    assert!(expanded.contains("struct Add"), "{expanded}");
}

#[test]
fn every_problem_is_reported_rather_than_only_the_first() {
    // A function with three mistakes should need one `cargo check`, not three.
    let refusals = refusals(
        quote! { nonsense = 1 },
        quote! { async fn add<T>(a: i64, a: i64) {} },
    );
    assert!(refusals.len() >= 3, "{refusals:?}");
}

#[test]
fn a_parameter_attribute_is_stripped_from_the_function_that_is_re_emitted() {
    // `#[duet(...)]` is inert and nothing downstream registers it. Leaving one
    // on the re-emitted signature would turn a working command into "cannot
    // find attribute `duet`".
    let expanded = expand(
        TokenStream::new(),
        quote! { fn label(#[duet(rename = "t")] title: String) -> String { title } },
    )
    .to_string();
    assert!(expanded.contains("fn label"), "{expanded}");
    assert!(
        !expanded.contains("# [duet"),
        "the inert attribute survived: {expanded}"
    );
    assert!(expanded.contains("\"t\""), "{expanded}");
}

#[test]
fn nothing_panics_on_malformed_input() {
    // A proc macro must never panic: "custom attribute panicked" carries the
    // macro's own backtrace and no span into the user's file, which is the worst
    // diagnostic in the language to be handed for a typo.
    let cases: [(TokenStream, TokenStream); 6] = [
        (TokenStream::new(), TokenStream::new()),
        (quote! { rename }, add()),
        (quote! { rename = }, add()),
        (quote! { crate = }, add()),
        (quote! { , , , }, add()),
        (quote! { rename = "a" }, quote! { fn f(: i64) {} }),
    ];
    for (attr, item) in cases {
        let expanded = expand(attr.clone(), item.clone()).to_string();
        assert!(
            expanded.contains("compile_error") || !expanded.is_empty(),
            "{attr} / {item}"
        );
    }
}
