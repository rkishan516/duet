//! Unit tests for the command half of the emit plan.

use super::*;
use duet_schema::FieldDef;

/// One command, with no return and no error unless the test says otherwise.
fn command(name: &str, params: Vec<FieldDef>) -> CommandDef {
    CommandDef {
        name: name.to_string(),
        params,
        returns: None,
        raises: None,
    }
}

/// The one command in a plan of one, or the reason there is none.
fn planned(command: CommandDef) -> PlannedCommand {
    let mut all = plan_commands(&[command]).unwrap_or_else(|e| panic!("should plan: {e}"));
    all.pop().expect("one command was planned")
}

#[test]
fn a_command_name_is_never_rewritten_and_a_method_name_always_is() {
    // The rule this whole module exists for, stated as an assertion. A method
    // called `documentsClose` reaching a host that owns `documents.close` is
    // not an error anywhere — it is a refusal at the far end of a call the
    // developer believed was typed.
    let planned = planned(command("documents.close", Vec::new()));
    assert_eq!(
        planned.name, "documents.close",
        "the wire name must survive"
    );
    assert_eq!(planned.method, "documentsClose");
}

#[test]
fn a_dot_separates_exactly_as_an_underscore_does() {
    for (name, method) in [
        ("subtract", "subtract"),
        ("reset_counter", "resetCounter"),
        ("documents.close", "documentsClose"),
        ("a.b.c", "aBC"),
        ("documents.rename_file", "documentsRenameFile"),
    ] {
        assert_eq!(planned(command(name, Vec::new())).method, method, "{name}");
    }
}

#[test]
fn two_commands_whose_names_camel_case_alike_are_refused() {
    // The collision at command scope. `documents.close` and `documents_close`
    // are different wire names and one Dart identifier, and nothing here can
    // know which of the two the developer meant.
    let error = plan_commands(&[
        command("documents.close", Vec::new()),
        command("documents_close", Vec::new()),
    ])
    .expect_err("two commands cannot share a method name");
    assert_eq!(
        error,
        EmitError::CommandCollision {
            method: "documentsClose".to_string()
        }
    );
    assert!(error.to_string().contains("documentsClose"));
}

#[test]
fn two_parameters_whose_keys_camel_case_alike_are_refused() {
    // The same collision one scope down, which a check written only at command
    // scope would miss entirely.
    let error = plan_commands(&[command(
        "save",
        vec![
            FieldDef::new("font_size", Ty::Int),
            FieldDef::new("fontSize", Ty::Int),
        ],
    )])
    .expect_err("two parameters cannot share a name");
    assert_eq!(
        error,
        EmitError::ParamCollision {
            command: "save".to_string(),
            accessor: "fontSize".to_string()
        }
    );
}

#[test]
fn a_parameter_key_is_never_rewritten_and_its_name_always_is() {
    let planned = planned(command("save", vec![FieldDef::new("font_size", Ty::Int)]));
    assert_eq!(
        planned.params[0].key, "font_size",
        "the args key must survive"
    );
    assert_eq!(planned.params[0].accessor, "fontSize");
}

#[test]
fn the_arguments_literal_is_written_in_wire_key_order() {
    // Declaration order is what the signature lists; byte order is what the
    // `args` map literal is written in, because that is what the wire's
    // canonical form is. The two are deliberately different here so a plan that
    // confused them fails.
    let planned = planned(command(
        "save",
        vec![
            FieldDef::new("zoom", Ty::Float),
            FieldDef::new("alpha", Ty::Int),
            FieldDef::new("Beta", Ty::Int),
        ],
    ));
    assert_eq!(
        planned
            .params
            .iter()
            .map(|p| p.key.as_str())
            .collect::<Vec<_>>(),
        ["zoom", "alpha", "Beta"],
        "the signature keeps declaration order"
    );
    assert_eq!(
        planned
            .sorted_params()
            .iter()
            .map(|p| p.key.as_str())
            .collect::<Vec<_>>(),
        ["Beta", "alpha", "zoom"],
        "the args literal is in byte order, so 'B' sorts before 'a'"
    );
}

#[test]
fn an_undeclared_return_or_error_becomes_dynamic_and_says_so() {
    // A command with no `returns` still answers — with null — so there has to
    // be a codec. `dynamic` is the one type that describes "whatever arrives",
    // and the two flags are what keep the stand-in distinguishable from a
    // schema that really declared `dynamic`.
    let none = planned(command("ping", Vec::new()));
    assert_eq!(none.returns, Ty::Dynamic);
    assert_eq!(none.raises, Ty::Dynamic);
    assert!(!none.declares_return);
    assert!(!none.declares_raise);

    let both = planned(CommandDef {
        name: "bump".to_string(),
        params: Vec::new(),
        returns: Some(Ty::Int),
        raises: Some(Ty::Named("Refusal".to_string())),
    });
    assert_eq!(both.returns, Ty::Int);
    assert_eq!(both.raises, Ty::Named("Refusal".to_string()));
    assert!(both.declares_return);
    assert!(both.declares_raise);

    let declared_dynamic = planned(CommandDef {
        name: "probe".to_string(),
        params: Vec::new(),
        returns: Some(Ty::Dynamic),
        raises: None,
    });
    assert_eq!(declared_dynamic.returns, Ty::Dynamic);
    assert!(
        declared_dynamic.declares_return,
        "a schema that really said `dynamic` must not look like one that said nothing"
    );
}

#[test]
fn an_optional_anywhere_in_a_signature_is_refused_by_name() {
    // A codec's type argument is non-nullable by design, and a command has no
    // second handle the way a struct field does. Every position is checked,
    // because a check on only one of the three would leave the others able to
    // emit `DuetCodec<int?>`, which does not compile. The last case buries the
    // optional inside a list, where a top-level-only check would miss it.
    for (what, command) in [
        (
            "path",
            CommandDef {
                name: "save".to_string(),
                params: vec![FieldDef::new("path", Ty::Str.optional())],
                returns: None,
                raises: None,
            },
        ),
        (
            "returns",
            CommandDef {
                name: "save".to_string(),
                params: Vec::new(),
                returns: Some(Ty::Int.optional()),
                raises: None,
            },
        ),
        (
            "raises",
            CommandDef {
                name: "save".to_string(),
                params: Vec::new(),
                returns: None,
                raises: Some(Ty::Str.optional()),
            },
        ),
        (
            "returns",
            CommandDef {
                name: "save".to_string(),
                params: Vec::new(),
                returns: Some(Ty::Int.optional().list()),
                raises: None,
            },
        ),
    ] {
        let error = match plan_commands(&[command]) {
            Err(error) => error,
            Ok(planned) => panic!("{what} should have been refused, got {planned:?}"),
        };
        match &error {
            EmitError::UnspellableCommandType {
                command,
                what: at,
                because,
            } => {
                assert_eq!(command, "save");
                assert_eq!(at, what);
                assert!(because.contains("non-nullable"), "{because}");
            }
            other => panic!("{what}: expected an unspellable type, got {other:?}"),
        }
    }
}

#[test]
fn an_unspellable_parameter_key_is_refused_rather_than_mangled() {
    for key in ["with space", "café", "2fast"] {
        let error = plan_commands(&[command("save", vec![FieldDef::new(key, Ty::Int)])])
            .expect_err("{key} should be refused");
        assert!(
            matches!(error, EmitError::UnspellableCommandType { .. }),
            "{key}: {error:?}"
        );
        assert!(error.to_string().contains("save"), "{key}: {error}");
    }
}

#[test]
fn a_parameter_or_method_may_not_be_called_client() {
    // `client` is the field every generated method body reads. In Dart a named
    // parameter shadows it for the whole body, so `client.invoke(...)` inside a
    // method with a `client` parameter would not compile — and in TypeScript it
    // would compile and be wrong in a different way. One rule, both languages.
    let method = plan_commands(&[command("client", Vec::new())])
        .expect_err("a command called `client` must be refused");
    assert!(
        matches!(method, EmitError::UnspellableCommand { .. }),
        "{method:?}"
    );

    let param = plan_commands(&[command("save", vec![FieldDef::new("client", Ty::Int)])])
        .expect_err("a parameter called `client` must be refused");
    assert!(
        matches!(param, EmitError::UnspellableCommandType { .. }),
        "{param:?}"
    );
}

#[test]
fn a_reserved_word_is_refused_in_both_positions() {
    for name in ["class", "void", "toString"] {
        assert!(
            plan_commands(&[command(name, Vec::new())]).is_err(),
            "a command called {name} must be refused"
        );
        assert!(
            plan_commands(&[command("save", vec![FieldDef::new(name, Ty::Int)])]).is_err(),
            "a parameter called {name} must be refused"
        );
    }
}

#[test]
fn a_schema_with_no_commands_plans_no_commands() {
    assert!(plan_commands(&[]).expect("an empty list plans").is_empty());
}
