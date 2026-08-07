//! Tests for [`CommandParam`](super::CommandParam): what it describes, what it
//! accepts, and what a refusal is allowed to say.

use super::*;
use duet_core::Value;

fn args(pairs: impl IntoIterator<Item = (&'static str, Value)>) -> Args {
    pairs
        .into_iter()
        .map(|(key, value)| (key.to_string(), value))
        .collect()
}

#[test]
fn a_parameter_is_described_by_its_own_shared_state_impl() {
    let mut registry = Registry::new();
    assert_eq!(<i64 as CommandParam>::param_ty(&mut registry), Ty::Int);
    assert_eq!(<String as CommandParam>::param_ty(&mut registry), Ty::Str);
    assert_eq!(
        <Vec<i64> as CommandParam>::param_ty(&mut registry),
        Ty::Int.list()
    );
    assert_eq!(
        <Option<String> as CommandParam>::param_ty(&mut registry),
        Ty::Str.optional()
    );
}

#[test]
fn a_present_argument_of_the_right_type_decodes() {
    let args = args([("a", Value::Int(7)), ("b", Value::Str("hi".into()))]);
    assert_eq!(<i64 as CommandParam>::from_args("a", &args), Ok(7));
    assert_eq!(
        <String as CommandParam>::from_args("b", &args),
        Ok("hi".to_string())
    );
}

#[test]
fn a_missing_argument_is_refused_and_the_message_names_it() {
    let refusal = <i64 as CommandParam>::from_args("by", &args([])).expect_err("absent");
    assert!(refusal.contains("\"by\""), "{refusal}");
    assert!(refusal.contains("missing"), "{refusal}");
}

#[test]
fn an_argument_of_the_wrong_type_is_refused_and_the_message_names_the_kind() {
    let refusal = <i64 as CommandParam>::from_args("by", &args([("by", Value::Str("7".into()))]))
        .expect_err("a string is not an int");
    assert!(refusal.contains("\"by\""), "{refusal}");
    assert!(refusal.contains("string"), "{refusal}");
}

#[test]
fn a_refusal_never_grows_with_the_argument_it_refused() {
    // Arguments are guest-chosen and unbounded. A one-megabyte string argument
    // must not become a one-megabyte reply, so the message names the *kind*
    // that arrived and never the value.
    let huge = "x".repeat(1_000_000);
    let refusal = <i64 as CommandParam>::from_args("by", &args([("by", Value::Str(huge.clone()))]))
        .expect_err("a string is not an int");
    assert!(
        refusal.len() < 300,
        "a 1 MB argument produced a {}-char refusal",
        refusal.len()
    );
    assert!(!refusal.contains(&huge[..64]), "{refusal}");
}

#[test]
fn a_nested_mismatch_says_where_inside_the_argument_it_was() {
    // A `Vec<i64>` argument holding a string is a different mistake from a
    // `Vec<i64>` argument that is not a list, and a developer fixing the call
    // needs to be told which.
    let refusal = <Vec<i64> as CommandParam>::from_args(
        "xs",
        &args([(
            "xs",
            Value::List(vec![Value::Int(1), Value::Str("2".into())]),
        )]),
    )
    .expect_err("element 1 is a string");
    assert!(refusal.contains("\"xs\""), "{refusal}");
    assert!(refusal.contains('1'), "the index must be named: {refusal}");
}

#[test]
fn a_null_is_a_present_argument_for_an_optional_and_a_refusal_for_anything_else() {
    // The distinction `Option` exists for, at the argument boundary: `None` is
    // a value that was supplied, not an argument that was left out.
    let nulled = args([("x", Value::Null)]);
    assert_eq!(
        <Option<i64> as CommandParam>::from_args("x", &nulled),
        Ok(None)
    );
    assert!(<i64 as CommandParam>::from_args("x", &nulled).is_err());
    assert!(<Option<i64> as CommandParam>::from_args("absent", &args([])).is_err());
}
