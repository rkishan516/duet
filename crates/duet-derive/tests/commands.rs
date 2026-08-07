//! `#[command]` against a live host: real arguments in, real replies out.
//!
//! # Why this file exists next to the golden expansion tests
//!
//! `src/command/generate_tests.rs` compares the macro's output to an expected
//! token stream. That proves the expansion is **stable**; it proves nothing
//! about whether it is **correct**, because the expected tokens were written by
//! reading the generator. A generator that decoded the second argument under
//! the first argument's key would produce a golden matching itself forever.
//!
//! Everything here runs the expansion instead. A `Runtime`, a real `Commands`
//! registry, real `invoke` text on the wire and the exact reply bytes back —
//! the same path `duet-host-stdio` puts a Dart or JavaScript guest on.
//! `subtract` rather than `add`, deliberately: subtraction is not commutative,
//! so a generated body that bound the two arguments the wrong way round answers
//! `-7` where `7` was expected. An `add` would answer correctly for a
//! completely broken binding.

use duet::{
    CommandContext, CommandEntry, Commands, Schema, SharedState, Value, command, commands, describe,
};
use duet_protocol::handle_text_with;
use duet_runtime::{NullSink, Runtime};

/// A domain error a command may raise, decoded by the guest as a value.
#[derive(SharedState, Debug, PartialEq)]
struct Refusal {
    code: String,
    short_by: i64,
}

/// A struct in argument position, so the schema records a `named` parameter.
#[derive(SharedState, Debug, PartialEq)]
struct Span {
    from: i64,
    to: i64,
}

#[command]
fn subtract(a: i64, b: i64) -> i64 {
    a.saturating_sub(b)
}

#[command]
fn raise() -> Result<(), Refusal> {
    Err(Refusal {
        code: "unlucky".to_string(),
        short_by: 42,
    })
}

#[command]
fn width(span: Span) -> i64 {
    span.to.saturating_sub(span.from)
}

#[command]
fn bump(ctx: &CommandContext, by: i64) -> Result<i64, Refusal> {
    let path = duet::Path::parse("count").map_err(|_| Refusal {
        code: "bad_path".to_string(),
        short_by: 0,
    })?;
    let current = match ctx.store().get(&path) {
        Ok(Some(Value::Int(n))) => n,
        _ => {
            return Err(Refusal {
                code: "absent".to_string(),
                short_by: 0,
            });
        }
    };
    let next = current.saturating_add(by);
    ctx.store()
        .set(&path, Value::Int(next))
        .map_err(|_| Refusal {
            code: "store".to_string(),
            short_by: 0,
        })?;
    Ok(next)
}

#[command(rename = "documents.reset")]
fn reset(ctx: &CommandContext) {
    if let Ok(path) = duet::Path::parse("count") {
        let _ = ctx.store().set(&path, Value::Int(0));
    }
}

#[command]
fn label(#[duet(rename = "window_title")] title: String) -> String {
    title
}

static COMMANDS: [CommandEntry; 6] = commands![subtract, raise, width, bump, reset, label];

/// A host holding `{"count": 1}` and the commands above.
fn host() -> Runtime {
    Runtime::spawn(Value::map([("count", Value::Int(1))]), NullSink)
}

/// One request line in, one reply line out — the guest's whole view.
fn ask(runtime: &Runtime, commands: &Commands, request: &str) -> String {
    handle_text_with(&runtime.handle(), duet::SubscriberId(1), commands, request)
}

#[test]
fn a_generated_command_binds_its_arguments_by_key_and_not_by_position() {
    // THE test. The arguments arrive in a map, so the wire has no positions at
    // all; a body that bound them in declaration order regardless of key would
    // pass for `add` and fail here. Both orderings are sent because a map's
    // iteration order is not the request's.
    let runtime = host();
    let commands = Commands::from_entries(&COMMANDS);
    for request in [
        r#"{"kind":"invoke","id":"1","command":"subtract","args":{"t":"m","v":{"a":{"t":"i","v":"10"},"b":{"t":"i","v":"3"}}}}"#,
        r#"{"kind":"invoke","id":"1","command":"subtract","args":{"t":"m","v":{"b":{"t":"i","v":"3"},"a":{"t":"i","v":"10"}}}}"#,
    ] {
        assert_eq!(
            ask(&runtime, &commands, request),
            r#"{"id":"1","kind":"returned","value":{"t":"i","v":"7"}}"#,
            "for {request}"
        );
    }
    runtime.shutdown().expect("shutdown should succeed");
}

#[test]
fn a_command_returning_err_reaches_the_guest_as_raised_and_structured() {
    let runtime = host();
    let commands = Commands::from_entries(&COMMANDS);
    assert_eq!(
        ask(
            &runtime,
            &commands,
            r#"{"kind":"invoke","id":"2","command":"raise","args":{"t":"m","v":{}}}"#
        ),
        r#"{"error":{"t":"m","v":{"code":{"t":"s","v":"unlucky"},"short_by":{"t":"i","v":"42"}}},"id":"2","kind":"raised"}"#
    );
    runtime.shutdown().expect("shutdown should succeed");
}

#[test]
fn a_command_reads_and_writes_the_same_store_a_guest_reads_through() {
    // The claim command RPC exists to have, through the declarative path: the
    // body's write is visible to an ordinary `get` afterwards. Asserted on the
    // store as well as on the reply, because a body whose `set` silently failed
    // would still return the right number.
    let runtime = host();
    let commands = Commands::from_entries(&COMMANDS);
    assert_eq!(
        ask(
            &runtime,
            &commands,
            r#"{"kind":"invoke","id":"3","command":"bump","args":{"t":"m","v":{"by":{"t":"i","v":"5"}}}}"#
        ),
        r#"{"id":"3","kind":"returned","value":{"t":"i","v":"6"}}"#
    );
    assert_eq!(
        ask(
            &runtime,
            &commands,
            r#"{"kind":"get","id":"4","path":"count"}"#
        ),
        r#"{"id":"4","kind":"value","value":{"t":"i","v":"6"}}"#
    );
    runtime.shutdown().expect("shutdown should succeed");
}

#[test]
fn a_command_with_no_return_type_still_answers_with_null() {
    let runtime = host();
    let commands = Commands::from_entries(&COMMANDS);
    assert_eq!(
        ask(
            &runtime,
            &commands,
            r#"{"kind":"invoke","id":"5","command":"documents.reset","args":{"t":"m","v":{}}}"#
        ),
        r#"{"id":"5","kind":"returned","value":{"t":"n"}}"#
    );
    assert_eq!(
        ask(
            &runtime,
            &commands,
            r#"{"kind":"get","id":"6","path":"count"}"#
        ),
        r#"{"id":"6","kind":"value","value":{"t":"i","v":"0"}}"#,
        "the renamed command must be the one that ran"
    );
    runtime.shutdown().expect("shutdown should succeed");
}

#[test]
fn a_renamed_argument_is_reached_under_its_new_key_and_not_its_old_one() {
    let runtime = host();
    let commands = Commands::from_entries(&COMMANDS);
    assert_eq!(
        ask(
            &runtime,
            &commands,
            r#"{"kind":"invoke","id":"7","command":"label","args":{"t":"m","v":{"window_title":{"t":"s","v":"draft"}}}}"#
        ),
        r#"{"id":"7","kind":"returned","value":{"t":"s","v":"draft"}}"#
    );
    let refused = ask(
        &runtime,
        &commands,
        r#"{"kind":"invoke","id":"8","command":"label","args":{"t":"m","v":{"title":{"t":"s","v":"draft"}}}}"#,
    );
    assert!(refused.contains("\"failed\""), "got {refused}");
    assert!(refused.contains("window_title"), "got {refused}");
    runtime.shutdown().expect("shutdown should succeed");
}

#[test]
fn a_struct_argument_is_decoded_whole() {
    let runtime = host();
    let commands = Commands::from_entries(&COMMANDS);
    assert_eq!(
        ask(
            &runtime,
            &commands,
            r#"{"kind":"invoke","id":"9","command":"width","args":{"t":"m","v":{"span":{"t":"m","v":{"from":{"t":"i","v":"2"},"to":{"t":"i","v":"9"}}}}}}"#
        ),
        r#"{"id":"9","kind":"returned","value":{"t":"i","v":"7"}}"#
    );
    runtime.shutdown().expect("shutdown should succeed");
}

#[test]
fn a_missing_or_mistyped_argument_is_failed_and_never_raised() {
    // The distinction the wire's two error kinds exist for: the call never got
    // as far as running, so it is a refusal. A guest that could not tell the
    // two apart could not decide whether retrying is safe.
    let runtime = host();
    let commands = Commands::from_entries(&COMMANDS);
    for request in [
        r#"{"kind":"invoke","id":"10","command":"subtract","args":{"t":"m","v":{"a":{"t":"i","v":"1"}}}}"#,
        r#"{"kind":"invoke","id":"10","command":"subtract","args":{"t":"m","v":{"a":{"t":"i","v":"1"},"b":{"t":"s","v":"2"}}}}"#,
    ] {
        let reply = ask(&runtime, &commands, request);
        assert!(
            reply.contains("\"kind\":\"failed\""),
            "for {request}: {reply}"
        );
        assert!(reply.contains("\\\"b\\\""), "for {request}: {reply}");
    }
    runtime.shutdown().expect("shutdown should succeed");
}

#[test]
fn a_command_is_still_an_ordinary_rust_function() {
    // The macro adds a description beside the function rather than replacing
    // it. A `#[command]` that consumed its item would break every in-process
    // caller, and the failure would be at the call site rather than here.
    assert_eq!(subtract(10, 3), 7);
    assert_eq!(width(Span { from: 2, to: 9 }), 7);
    assert_eq!(label("draft".to_string()), "draft");
}

#[test]
fn the_schema_records_every_command_and_the_types_they_name() {
    // The other half of the expansion, and the half a golden cannot check
    // against reality: `describe` and `run` come from one signature, so a
    // schema that disagreed with the behaviour above would be a generated
    // client compiled against a shape the host does not serve.
    let schema = Schema::of_with_commands::<i64>(|registry| describe(&COMMANDS, registry))
        .expect("a valid schema");
    let rendered = schema.render();

    // Sorted by name, whatever order `commands![…]` listed them in.
    let names: Vec<&str> = schema.commands().iter().map(|c| c.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "bump",
            "documents.reset",
            "label",
            "raise",
            "subtract",
            "width"
        ]
    );

    // The four return shapes, each spelled as `CommandDef` documents.
    assert!(
        rendered.contains(
            "{\n      \"name\": \"subtract\",\n      \"params\": [\n        \
             {\"key\": \"a\", \"type\": {\"kind\": \"int\"}},\n        \
             {\"key\": \"b\", \"type\": {\"kind\": \"int\"}}\n      ],\n      \
             \"returns\": {\"kind\": \"int\"}\n    }"
        ),
        "{rendered}"
    );
    assert!(
        rendered.contains("{\n      \"name\": \"documents.reset\",\n      \"params\": []\n    }"),
        "a command with no return type omits `returns`: {rendered}"
    );
    assert!(
        rendered.contains(
            "{\n      \"name\": \"raise\",\n      \"params\": [],\n      \
             \"raises\": {\"kind\": \"named\", \"name\": \"Refusal\"}\n    }"
        ),
        "`Result<(), E>` records `raises` and no `returns`: {rendered}"
    );

    // The context parameter is not an argument, and does not reach the schema.
    let bump = schema
        .commands()
        .iter()
        .find(|c| c.name == "bump")
        .expect("bump is registered");
    let keys: Vec<&str> = bump.params.iter().map(|p| p.key.as_str()).collect();
    assert_eq!(keys, ["by"], "`&CommandContext` is not an argument");

    // A type only a command mentions still lands in `types`.
    assert!(
        schema.types().iter().any(|t| t.name == "Refusal"),
        "an error type nothing else reaches must still be defined: {rendered}"
    );
    assert!(schema.types().iter().any(|t| t.name == "Span"));
}
