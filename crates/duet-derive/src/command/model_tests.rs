//! Tests for what `#[command]` understood — and for every shape it refuses.

use super::*;
use quote::{ToTokens as _, quote};

/// Parses a function with no `#[command(...)]` arguments.
fn model(item: TokenStream) -> Result<Model, Vec<String>> {
    with_attr(TokenStream::new(), item)
}

/// Parses a function with the given `#[command(...)]` arguments.
fn with_attr(attr: TokenStream, item: TokenStream) -> Result<Model, Vec<String>> {
    let parsed: syn::ItemFn = syn::parse2(item).expect("the test item should parse as a function");
    Model::parse(attr, &parsed)
        .map_err(|errors| errors.into_iter().map(|e| e.to_string()).collect())
}

/// The one complaint `item` produced, or a panic naming what happened instead.
fn one_refusal(item: TokenStream) -> String {
    let problems = model(item).err().expect("this shape should be refused");
    assert_eq!(problems.len(), 1, "{problems:?}");
    problems.into_iter().next().unwrap_or_default()
}

/// Every value parameter's key, in declaration order.
fn keys(model: &Model) -> Vec<&str> {
    model
        .params
        .iter()
        .filter_map(|param| match param {
            Param::Value { key, .. } => Some(key.as_str()),
            Param::Context { .. } => None,
        })
        .collect()
}

#[test]
fn a_plain_function_is_read_as_its_name_its_keys_and_its_return() {
    let model = model(quote! { fn subtract(a: i64, b: i64) -> i64 { a - b } })
        .expect("a plain function is a command");
    assert_eq!(model.name, "subtract");
    assert_eq!(keys(&model), ["a", "b"]);
    assert_eq!(model.ret.to_token_stream().to_string(), "i64");
    assert_eq!(model.krate.to_token_stream().to_string(), ":: duet");
}

#[test]
fn no_return_type_reads_as_the_unit_type() {
    // Which is what `CommandReturn`'s `()` impl is for. Spelling it here rather
    // than in the generator keeps the generator free of branches.
    let model = model(quote! { fn reset() {} }).expect("a command may return nothing");
    assert_eq!(model.ret.to_token_stream().to_string(), "()");
    assert!(model.params.is_empty());
}

#[test]
fn a_reference_parameter_is_the_context_and_occupies_no_key() {
    let model = model(quote! { fn bump(ctx: &CommandContext, by: i64) {} })
        .expect("a context parameter is legal");
    assert_eq!(keys(&model), ["by"]);
    assert!(matches!(model.params[0], Param::Context { .. }));
    assert!(matches!(model.params[1], Param::Value { .. }));
}

#[test]
fn the_context_may_be_written_in_any_position() {
    // Nothing here depends on it being first: the binding order in `run` is the
    // declaration order, whatever that is.
    let model = model(quote! { fn bump(by: i64, ctx: &CommandContext) {} }).expect("legal");
    assert!(matches!(model.params[0], Param::Value { .. }));
    assert!(matches!(model.params[1], Param::Context { .. }));
}

#[test]
fn a_rename_settles_the_command_name_and_the_argument_keys() {
    let model = with_attr(
        quote! { rename = "documents.add" },
        quote! { fn add(#[duet(rename = "window_title")] title: String) {} },
    )
    .expect("both renames are legal");
    assert_eq!(model.name, "documents.add");
    assert_eq!(keys(&model), ["window_title"]);
}

#[test]
fn a_raw_identifier_loses_its_prefix_in_both_positions() {
    // `r#` is Rust syntax for "this word is a keyword", not part of the name.
    // Leaving it on would put `r#type` on the wire and in every generated Dart
    // accessor.
    let model = model(quote! { fn r#move(r#type: i64) {} }).expect("raw identifiers are legal");
    assert_eq!(model.name, "move");
    assert_eq!(keys(&model), ["type"]);
}

#[test]
fn a_crate_attribute_redirects_every_generated_path() {
    let model = with_attr(quote! { crate = ::my_reexport }, quote! { fn f() {} })
        .expect("a crate path is legal");
    assert_eq!(model.krate.to_token_stream().to_string(), ":: my_reexport");
}

// --- Refusals ---

#[test]
fn an_async_function_is_refused_and_the_message_names_the_alternative() {
    let refusal = one_refusal(quote! { async fn f() {} });
    assert!(refusal.contains("async fn"), "{refusal}");
    assert!(refusal.contains("spawn a thread"), "{refusal}");
}

#[test]
fn an_unsafe_function_is_refused() {
    let refusal = one_refusal(quote! { unsafe fn f() {} });
    assert!(refusal.contains("unsafe fn"), "{refusal}");
}

#[test]
fn a_generic_function_is_refused() {
    for item in [
        quote! { fn f<T>(a: T) {} },
        quote! { fn f<'a>(a: &'a i64) {} },
        quote! { fn f(a: i64) where i64: Copy {} },
    ] {
        let refusal = one_refusal(item.clone());
        assert!(refusal.contains("generic function"), "{item}: {refusal}");
    }
}

#[test]
fn a_method_with_a_receiver_is_refused() {
    for item in [
        quote! { fn f(&self) {} },
        quote! { fn f(self) {} },
        quote! { fn f(&mut self, a: i64) {} },
    ] {
        let refusal = one_refusal(item.clone());
        assert!(
            refusal.contains("cannot describe a method"),
            "{item}: {refusal}"
        );
    }
}

#[test]
fn a_parameter_that_is_not_a_plain_name_is_refused() {
    // An argument key is a parameter *name*, so anything without one has no key.
    for item in [
        quote! { fn f((a, b): (i64, i64)) {} },
        quote! { fn f(_: i64) {} },
        quote! { fn f(ref a: i64) {} },
    ] {
        let refusal = one_refusal(item.clone());
        assert!(refusal.contains("plain name"), "{item}: {refusal}");
    }
}

#[test]
fn two_parameters_on_one_key_are_refused() {
    let refusal = one_refusal(quote! { fn f(a: i64, #[duet(rename = "a")] b: i64) {} });
    assert!(refusal.contains("share the wire key"), "{refusal}");
}

#[test]
fn two_parameters_that_camel_case_alike_are_refused() {
    // `font_size` and `fontSize` are two distinct argument keys and one Dart
    // accessor, so a generated client would have two parameters of one name.
    let refusal = one_refusal(quote! { fn f(font_size: i64, fontSize: i64) {} });
    assert!(refusal.contains("fontSize"), "{refusal}");
    assert!(refusal.contains("accessor"), "{refusal}");
}

#[test]
fn an_argument_key_that_is_not_one_path_segment_is_refused() {
    let refusal = one_refusal(quote! { fn f(#[duet(rename = "a.b")] a: i64) {} });
    assert!(refusal.contains("not a legal wire key"), "{refusal}");
}

#[test]
fn a_command_name_the_schema_would_refuse_is_a_compile_error_instead() {
    // Checked against `duet_schema::is_legal_command_name` — the real predicate,
    // not a second copy of the grammar. `Schema::of_with_commands` would refuse
    // the same name at startup; refusing it here names the literal to change.
    for name in ["2fast", "documents.", ".rename", "has space", "_leading"] {
        let problems = with_attr(quote! { rename = #name }, quote! { fn f() {} })
            .err()
            .unwrap_or_else(|| panic!("{name} should be refused"));
        assert!(
            problems
                .iter()
                .any(|p| p.contains("not a legal command name")),
            "{name}: {problems:?}"
        );
    }
}

#[test]
fn a_legal_dotted_name_is_accepted() {
    let model = with_attr(quote! { rename = "documents.rename" }, quote! { fn f() {} })
        .expect("a dotted name is legal");
    assert_eq!(model.name, "documents.rename");
}

#[test]
fn a_function_whose_own_name_is_illegal_is_refused_without_a_rename() {
    let refusal = one_refusal(quote! { fn _leading() {} });
    assert!(refusal.contains("not a legal command name"), "{refusal}");
}

#[test]
fn renaming_the_context_is_refused_because_it_occupies_no_key() {
    let refusal = one_refusal(quote! { fn f(#[duet(rename = "x")] ctx: &CommandContext) {} });
    assert!(refusal.contains("is not an argument"), "{refusal}");
}

#[test]
fn a_variadic_function_is_refused() {
    let refusal = one_refusal(quote! { fn f(a: i64, ...) {} });
    assert!(refusal.contains("variadic"), "{refusal}");
}

#[test]
fn stripping_removes_only_the_duet_attributes() {
    let parsed: syn::ItemFn = syn::parse2(quote! {
        fn f(#[duet(rename = "x")] #[allow(unused)] a: i64) {}
    })
    .expect("the test item should parse");
    let stripped = stripped(&parsed).to_token_stream().to_string();
    assert!(!stripped.contains("duet"), "{stripped}");
    assert!(stripped.contains("allow"), "{stripped}");
}
