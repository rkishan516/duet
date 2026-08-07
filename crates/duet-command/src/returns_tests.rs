//! Tests for [`CommandReturn`](super::CommandReturn): the four shapes, their
//! schema spellings, and their outcomes.

use super::*;
use duet_schema::{DecodeError, NotNullable, SharedState};

/// A domain error, as a `#[command]` returning `Result` would use.
#[derive(Debug, PartialEq)]
struct Refusal {
    code: String,
}

impl SharedState for Refusal {
    fn to_value(&self) -> Value {
        Value::map([("code", Value::Str(self.code.clone()))])
    }

    fn from_value(value: &Value) -> Result<Refusal, DecodeError> {
        match value {
            Value::Map(entries) => match entries.get("code") {
                Some(Value::Str(code)) => Ok(Refusal { code: code.clone() }),
                _ => Err(DecodeError::missing_field("Refusal", "code")),
            },
            other => Err(DecodeError::wrong_type("Refusal", other)),
        }
    }

    fn schema(registry: &mut Registry) -> Ty {
        registry.define::<Self>("Refusal", |_| {
            vec![duet_schema::FieldDef::new("code", Ty::Str)]
        })
    }
}

impl NotNullable for Refusal {}

fn refusal() -> Refusal {
    Refusal {
        code: "unlucky".to_string(),
    }
}

/// `(returns, raises)` for `R`, described into a throwaway registry.
fn described<R: CommandReturn<M>, M>() -> (Option<Ty>, Option<Ty>) {
    let mut registry = Registry::new();
    (
        command_returns::<R, M>(&mut registry),
        command_raises::<R, M>(&mut registry),
    )
}

#[test]
fn a_plain_value_returns_its_type_and_raises_nothing() {
    assert_eq!(described::<i64, _>(), (Some(Ty::Int), None));
    assert_eq!(described::<String, _>(), (Some(Ty::Str), None));
    assert_eq!(
        into_outcome(7i64),
        Outcome::Returned(Value::Int(7)),
        "a plain value is always a `returned`"
    );
}

#[test]
fn nothing_returns_nothing_and_still_answers_with_null() {
    // The schema records no return *type*; the wire still carries a value,
    // because every `invoke` is answered. Both halves are asserted, because a
    // `returns` of `None` paired with a `Returned(Int)` would be a generated
    // client decoding a node the schema says is not there.
    assert_eq!(described::<(), _>(), (None, None));
    assert_eq!(into_outcome(()), Outcome::Returned(Value::Null));
}

#[test]
fn a_result_returns_its_ok_type_and_raises_its_error_type() {
    let (returns, raises) = described::<Result<i64, Refusal>, _>();
    assert_eq!(returns, Some(Ty::Int));
    assert_eq!(raises, Some(Ty::Named("Refusal".to_string())));
    assert_eq!(
        into_outcome(Ok::<i64, Refusal>(7)),
        Outcome::Returned(Value::Int(7))
    );
    assert_eq!(
        into_outcome(Err::<i64, Refusal>(refusal())),
        Outcome::Raised(Value::map([("code", Value::Str("unlucky".into()))])),
        "an Err becomes a `raised`, structured — never a `failed`"
    );
}

#[test]
fn a_result_with_no_ok_type_still_raises_its_error_type() {
    let (returns, raises) = described::<Result<(), Refusal>, _>();
    assert_eq!(returns, None);
    assert_eq!(raises, Some(Ty::Named("Refusal".to_string())));
    assert_eq!(
        into_outcome(Ok::<(), Refusal>(())),
        Outcome::Returned(Value::Null)
    );
    assert_eq!(
        into_outcome(Err::<(), Refusal>(refusal())),
        Outcome::Raised(Value::map([("code", Value::Str("unlucky".into()))]))
    );
}

#[test]
fn describing_a_return_registers_the_types_it_names() {
    // The reason `describe` takes a registry at all: an error type is very
    // often reachable from nothing else in the schema, so if describing a
    // return did not register it, the schema would name a struct it never
    // defines.
    let schema = duet_schema::Schema::of_with_commands::<i64>(|registry| {
        vec![duet_schema::CommandDef {
            name: "save".to_string(),
            params: Vec::new(),
            returns: command_returns::<Result<(), Refusal>, _>(registry),
            raises: command_raises::<Result<(), Refusal>, _>(registry),
        }]
    })
    .expect("a valid schema");
    assert_eq!(schema.types().len(), 1);
    assert_eq!(schema.types()[0].name, "Refusal");
}

#[test]
fn the_four_impls_are_selected_without_an_annotation() {
    // The inference property the markers exist to make possible, and the one
    // `crates/duet-schema/tests/rejections.rs` guards from the other side. A
    // `SharedState` impl for `()` or `Result` would make this file stop
    // compiling with "type annotations needed" — which is why this is written
    // as four calls with `_` rather than with the markers named.
    fn accepts<R: CommandReturn<M>, M>() {}
    accepts::<i64, _>();
    accepts::<(), _>();
    accepts::<Result<i64, Refusal>, _>();
    accepts::<Result<(), Refusal>, _>();
}
