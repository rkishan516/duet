//! Tests for [`CommandEntry`](super::CommandEntry), [`describe`](super::describe)
//! and [`commands!`].
//!
//! Written **without** `#[command]`: this crate sits below `duet-derive` and
//! cannot depend on it, and that is worth something on its own — the impls here
//! are what the macro has to produce, spelled out, so a reader can see the
//! shape the expansion is aiming at and this crate's half is tested even if the
//! macro is broken.

use super::*;
use crate::{CommandParam, Commands, command_raises, command_returns, into_outcome};
use duet_core::{SubscriberId, Value};
use duet_protocol::{Request, RequestId, Response, dispatch_with};
use duet_runtime::{NullSink, Runtime, StoreHandle};
use duet_schema::{FieldDef, Ty};

/// `add(a, b) -> a + b`, hand-written exactly as `#[command]` would write it.
#[allow(non_camel_case_types)]
struct add {}

fn add(a: i64, b: i64) -> i64 {
    a.saturating_add(b)
}

impl Command for add {
    const NAME: &'static str = "add";

    fn describe(registry: &mut Registry) -> CommandDef {
        CommandDef {
            name: "add".to_string(),
            params: Vec::from([
                FieldDef::new("a", <i64 as CommandParam>::param_ty(registry)),
                FieldDef::new("b", <i64 as CommandParam>::param_ty(registry)),
            ]),
            returns: command_returns::<i64, _>(registry),
            raises: command_raises::<i64, _>(registry),
        }
    }

    fn run(args: Args, context: &CommandContext) -> Outcome {
        let a = match <i64 as CommandParam>::from_args("a", &args) {
            Ok(value) => value,
            Err(why) => return Outcome::Refused(why),
        };
        let b = match <i64 as CommandParam>::from_args("b", &args) {
            Ok(value) => value,
            Err(why) => return Outcome::Refused(why),
        };
        let _ = context;
        into_outcome(add(a, b))
    }
}

/// A command with no arguments and no result, to prove the empty shapes work.
#[allow(non_camel_case_types)]
struct ping {}

fn ping() {}

impl Command for ping {
    const NAME: &'static str = "ping";

    fn describe(registry: &mut Registry) -> CommandDef {
        CommandDef {
            name: "ping".to_string(),
            params: Vec::new(),
            returns: command_returns::<(), _>(registry),
            raises: command_raises::<(), _>(registry),
        }
    }

    fn run(_args: Args, _context: &CommandContext) -> Outcome {
        // `#[allow]` rather than a rewrite: this is precisely what `#[command]`
        // expands to for a function with no return type, and the expansion
        // carries the same allow. Calling the body and lowering its `()` is the
        // whole point.
        #[allow(clippy::unit_arg)]
        into_outcome(ping())
    }
}

/// The whole registration, as an embedder writes it.
static COMMANDS: [CommandEntry; 2] = commands![add, ping];

#[test]
fn a_static_table_needs_no_runtime_construction() {
    // The property `CommandEntry::of` is `const` for. If it stopped being
    // `const`, this `static` would not compile — so the assertion is really the
    // declaration above, and this only pins what it holds.
    assert_eq!(COMMANDS.len(), 2);
    assert_eq!(COMMANDS[0].name(), "add");
    assert_eq!(COMMANDS[1].name(), "ping");
}

#[test]
fn describing_a_table_yields_one_definition_per_entry() {
    let mut registry = Registry::new();
    let described = describe(&COMMANDS, &mut registry);
    assert_eq!(
        described,
        vec![
            CommandDef {
                name: "add".to_string(),
                params: vec![FieldDef::new("a", Ty::Int), FieldDef::new("b", Ty::Int)],
                returns: Some(Ty::Int),
                raises: None,
            },
            CommandDef {
                name: "ping".to_string(),
                params: Vec::new(),
                returns: None,
                raises: None,
            },
        ]
    );
}

#[test]
fn a_described_table_becomes_a_schema_alongside_the_root() {
    // The seam between this crate and `duet-schema`: one registry, both halves.
    let schema =
        duet_schema::Schema::of_with_commands::<i64>(|registry| describe(&COMMANDS, registry))
            .expect("a valid schema");
    assert!(
        schema.render().contains("\"name\": \"add\""),
        "{}",
        schema.render()
    );
    assert_eq!(schema.commands().len(), 2);
}

#[test]
fn a_registry_built_from_the_table_serves_the_commands_it_holds() {
    let runtime = Runtime::spawn(Value::Null, NullSink);
    let commands = Commands::from_entries(&COMMANDS);
    assert_eq!(commands.names(), ["add", "ping"]);

    assert_eq!(
        dispatch_with(
            &runtime.handle(),
            SubscriberId(1),
            &commands,
            Request::Invoke {
                id: RequestId(1),
                command: "add".to_string(),
                args: Args::from([
                    ("a".to_string(), Value::Int(2)),
                    ("b".to_string(), Value::Int(3)),
                ]),
            },
        ),
        Response::Returned {
            id: RequestId(1),
            value: Value::Int(5)
        }
    );
    assert_eq!(
        dispatch_with(
            &runtime.handle(),
            SubscriberId(1),
            &commands,
            Request::Invoke {
                id: RequestId(2),
                command: "ping".to_string(),
                args: Args::new(),
            },
        ),
        Response::Returned {
            id: RequestId(2),
            value: Value::Null
        },
        "a command with no result still answers"
    );
    runtime.shutdown().expect("shutdown should succeed");
}

#[test]
fn a_missing_argument_reaches_the_guest_as_failed_not_raised() {
    // The half of the contract `CommandParam` owns, measured through the wire
    // rather than at the trait: the call never got as far as running, so it is
    // a refusal.
    let runtime = Runtime::spawn(Value::Null, NullSink);
    let commands = Commands::from_entries(&COMMANDS);
    match dispatch_with(
        &runtime.handle(),
        SubscriberId(1),
        &commands,
        Request::Invoke {
            id: RequestId(3),
            command: "add".to_string(),
            args: Args::from([("a".to_string(), Value::Int(2))]),
        },
    ) {
        Response::Failed { id, message } => {
            assert_eq!(id, RequestId(3));
            assert!(message.contains("\"b\""), "got {message}");
        }
        other => panic!("expected Failed, got {other:?}"),
    }
    runtime.shutdown().expect("shutdown should succeed");
}

#[test]
fn a_body_reaches_the_store_through_the_context_it_is_handed() {
    // The reentrancy case, through the declarative path: a body runs on the
    // caller's thread, so the `StoreHandle` the context carries works normally.
    #[allow(non_camel_case_types)]
    struct bump {}

    fn bump(ctx: &CommandContext) -> Result<i64, String> {
        let path = duet_core::Path::parse("count").map_err(|e| e.to_string())?;
        let current = match ctx.store().get(&path) {
            Ok(Some(Value::Int(n))) => n,
            other => return Err(format!("unreadable: {other:?}")),
        };
        ctx.store()
            .set(&path, Value::Int(current + 1))
            .map_err(|e| e.to_string())?;
        Ok(current + 1)
    }

    impl Command for bump {
        const NAME: &'static str = "bump";

        fn describe(registry: &mut Registry) -> CommandDef {
            CommandDef {
                name: "bump".to_string(),
                params: Vec::new(),
                returns: command_returns::<Result<i64, String>, _>(registry),
                raises: command_raises::<Result<i64, String>, _>(registry),
            }
        }

        fn run(_args: Args, context: &CommandContext) -> Outcome {
            into_outcome(bump(context))
        }
    }

    static BUMP: [CommandEntry; 1] = commands![bump];

    let runtime = Runtime::spawn(Value::map([("count", Value::Int(1))]), NullSink);
    let handle: StoreHandle = runtime.handle();
    let commands = Commands::from_entries(&BUMP);
    for (id, expected) in [(4u64, 2i64), (5, 3)] {
        assert_eq!(
            dispatch_with(
                &handle,
                SubscriberId(1),
                &commands,
                Request::Invoke {
                    id: RequestId(id),
                    command: "bump".to_string(),
                    args: Args::new(),
                },
            ),
            Response::Returned {
                id: RequestId(id),
                value: Value::Int(expected)
            }
        );
    }
    let path = duet_core::Path::parse("count").expect("test path should parse");
    assert_eq!(
        handle.get(&path).expect("read should succeed"),
        Some(Value::Int(3)),
        "every write the body made must have landed"
    );
    runtime.shutdown().expect("shutdown should succeed");
}

#[test]
fn an_empty_table_is_a_registry_that_refuses_everything() {
    static NONE: [CommandEntry; 0] = commands![];
    let commands = Commands::from_entries(&NONE);
    assert!(commands.is_empty());
}
