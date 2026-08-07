//! Tests for [`super::commands`], driven the way the wire drives them.
//!
//! Every case goes through [`duet_protocol::handle_text_with`] rather than
//! calling a handler directly: the bytes are what the Dart and TypeScript live
//! conformance runs assert, and a test that called `subtract` as a Rust function
//! would agree with itself while the envelope around it was wrong.

use super::*;
use duet_core::SubscriberId;
use duet_runtime::{NullSink, Runtime};

/// Serves one request line against a store seeded with `root`, returning the
/// reply text and the handle, so a test can look at what the command did.
fn serve(root: Value, request: &str) -> (String, Runtime) {
    let runtime = Runtime::spawn(root, NullSink);
    let reply =
        duet_protocol::handle_text_with(&runtime.handle(), SubscriberId(1), &commands(), request);
    (reply, runtime)
}

fn seeded() -> Value {
    Value::map([("counter", Value::Int(0)), ("title", Value::Str("".into()))])
}

fn stop(runtime: Runtime) {
    runtime.shutdown().expect("the store should stop");
}

#[test]
fn the_registry_holds_exactly_the_three_documented_commands() {
    // A registry is the authorization boundary, so what is in it is part of
    // this host's contract rather than an implementation detail. A fourth
    // command appearing here without a guest case is what this catches.
    assert_eq!(commands().names(), [BUMP, RAISE, PING, SUBTRACT]);
}

#[test]
fn subtract_answers_the_exact_bytes_a_guest_decodes() {
    let (reply, runtime) = serve(
        seeded(),
        r#"{"kind":"invoke","id":"1","command":"subtract","args":{"t":"m","v":{"a":{"t":"i","v":"10"},"b":{"t":"i","v":"3"}}}}"#,
    );
    assert_eq!(
        reply,
        r#"{"id":"1","kind":"returned","value":{"t":"i","v":"7"}}"#
    );
    stop(runtime);
}

#[test]
fn subtract_is_not_commutative_so_a_swapped_encoder_cannot_pass() {
    // The reason this command is `subtract` and not `add`. Both orderings are
    // served, and they must disagree — an `add` would answer identically to a
    // guest whose argument encoder had swapped the two names, and every
    // conformance assertion built on it would be vacuous.
    let (forward, a) = serve(
        seeded(),
        r#"{"kind":"invoke","id":"1","command":"subtract","args":{"t":"m","v":{"a":{"t":"i","v":"10"},"b":{"t":"i","v":"3"}}}}"#,
    );
    let (swapped, b) = serve(
        seeded(),
        r#"{"kind":"invoke","id":"1","command":"subtract","args":{"t":"m","v":{"a":{"t":"i","v":"3"},"b":{"t":"i","v":"10"}}}}"#,
    );
    assert_eq!(
        forward,
        r#"{"id":"1","kind":"returned","value":{"t":"i","v":"7"}}"#
    );
    assert_eq!(
        swapped,
        r#"{"id":"1","kind":"returned","value":{"t":"i","v":"-7"}}"#
    );
    stop(a);
    stop(b);
}

#[test]
fn a_missing_or_mistyped_argument_is_failed_and_never_raised() {
    // The host would not run it, so this is a refusal — not a command that ran
    // and returned an error. A guest tells the two apart by the envelope kind,
    // and this is where that distinction is decided.
    for args in [
        r#"{"t":"m","v":{}}"#,
        r#"{"t":"m","v":{"a":{"t":"i","v":"1"}}}"#,
        r#"{"t":"m","v":{"a":{"t":"s","v":"1"},"b":{"t":"i","v":"1"}}}"#,
        // The right values under the wrong names: exactly what a guest whose
        // encoder renamed its arguments would send.
        r#"{"t":"m","v":{"x":{"t":"i","v":"1"},"y":{"t":"i","v":"1"}}}"#,
    ] {
        let request = format!(r#"{{"kind":"invoke","id":"1","command":"subtract","args":{args}}}"#);
        let (reply, runtime) = serve(seeded(), &request);
        let parsed: serde_json::Value =
            serde_json::from_str(&reply).expect("the reply should be JSON");
        assert_eq!(parsed["kind"], "failed", "{args} produced {reply}");
        assert_eq!(parsed["id"], "1", "the guest must fail that one call");
        stop(runtime);
    }
}

#[test]
fn a_refusal_never_echoes_an_arguments_value() {
    // Arguments are guest-chosen and unbounded. A refusal that rendered one
    // turns a one-megabyte argument into a one-megabyte reply, on a path that
    // exists because something already went wrong.
    let huge = "z".repeat(1_000_000);
    let request = format!(
        r#"{{"kind":"invoke","id":"1","command":"subtract","args":{{"t":"m","v":{{"a":{{"t":"s","v":"{huge}"}},"b":{{"t":"i","v":"1"}}}}}}}}"#
    );
    let (reply, runtime) = serve(seeded(), &request);
    assert!(
        reply.len() < 300,
        "a 1 MB argument produced a {}-byte reply",
        reply.len()
    );
    assert!(reply.contains("failed"), "{reply}");
    stop(runtime);
}

#[test]
fn a_refusal_names_the_kind_that_arrived_for_every_kind_the_wire_has() {
    // `kind_of` is what keeps a refusal informative without echoing a value.
    // Every arm of it is reachable from a guest — an argument may be any value
    // — so every arm is driven here rather than the two that happen to be easy.
    for (payload, kind) in [
        (r#"{"t":"n"}"#, "null"),
        (r#"{"t":"bool","v":true}"#, "bool"),
        (r#"{"t":"f","v":1.5}"#, "float"),
        (r#"{"t":"s","v":"1"}"#, "string"),
        (r#"{"t":"b","v":"AQID"}"#, "bytes"),
        (r#"{"t":"l","v":[]}"#, "list"),
        (r#"{"t":"m","v":{}}"#, "map"),
    ] {
        let (reply, runtime) = serve(
            seeded(),
            &format!(
                r#"{{"kind":"invoke","id":"1","command":"subtract","args":{{"t":"m","v":{{"a":{payload},"b":{{"t":"i","v":"1"}}}}}}}}"#
            ),
        );
        let parsed: serde_json::Value =
            serde_json::from_str(&reply).expect("the reply should be JSON");
        assert_eq!(
            parsed["message"],
            format!("argument \"a\" must be an integer, got a {kind}")
        );
        stop(runtime);
    }
}

#[test]
fn bump_refuses_a_missing_or_mistyped_path_argument() {
    for (args, message) in [
        (
            r#"{"t":"m","v":{"by":{"t":"i","v":"1"}}}"#,
            "argument \"path\" is missing",
        ),
        (
            r#"{"t":"m","v":{"by":{"t":"i","v":"1"},"path":{"t":"i","v":"1"}}}"#,
            "argument \"path\" must be a string, got a int",
        ),
        (
            r#"{"t":"m","v":{"path":{"t":"s","v":"counter"}}}"#,
            "argument \"by\" is missing",
        ),
    ] {
        let (reply, runtime) = serve(
            seeded(),
            &format!(r#"{{"kind":"invoke","id":"1","command":"bump","args":{args}}}"#),
        );
        let parsed: serde_json::Value =
            serde_json::from_str(&reply).expect("the reply should be JSON");
        assert_eq!(parsed["kind"], "failed", "{args} produced {reply}");
        assert_eq!(parsed["message"], message);
        stop(runtime);
    }
}

#[test]
fn a_command_whose_store_has_gone_raises_rather_than_panicking() {
    // The last arm a guest can reach: the core thread is gone, so the handle
    // answers `Err`. A body that unwrapped it would take the host down on a
    // path a guest can cause simply by racing shutdown.
    let runtime = Runtime::spawn(seeded(), NullSink);
    let handle = runtime.handle();
    runtime.shutdown().expect("the store should stop");

    let reply = duet_protocol::handle_text_with(
        &handle,
        SubscriberId(1),
        &commands(),
        r#"{"kind":"invoke","id":"1","command":"bump","args":{"t":"m","v":{"by":{"t":"i","v":"1"},"path":{"t":"s","v":"counter"}}}}"#,
    );
    let parsed: serde_json::Value = serde_json::from_str(&reply).expect("the reply should be JSON");
    assert_eq!(parsed["kind"], "raised", "{reply}");
    assert_eq!(parsed["error"]["v"]["code"]["v"], "store");
}

#[test]
fn raise_answers_the_exact_structured_error() {
    // The bytes a guest decodes into its own error type. Pinned whole, because
    // "some raised error arrived" would pass against a command that lost every
    // field of it.
    let (reply, runtime) = serve(
        seeded(),
        r#"{"kind":"invoke","id":"2","command":"raise","args":{"t":"m","v":{}}}"#,
    );
    assert_eq!(
        reply,
        r#"{"error":{"t":"m","v":{"code":{"t":"s","v":"unlucky"},"short_by":{"t":"i","v":"42"}}},"id":"2","kind":"raised"}"#
    );
    stop(runtime);
}

#[test]
fn bump_reads_and_writes_the_same_store_the_guest_reads() {
    // The claim commands exist to make: a command body and a guest's `get`
    // address one store. Asserted on the reply AND on a subsequent read through
    // the wire, because a body whose `set` silently failed would still return
    // the right number.
    let runtime = Runtime::spawn(seeded(), NullSink);
    let handle = runtime.handle();
    let commands = commands();

    for (id, expected) in [(1u32, "5"), (2, "10")] {
        let reply = duet_protocol::handle_text_with(
            &handle,
            SubscriberId(1),
            &commands,
            &format!(
                r#"{{"kind":"invoke","id":"{id}","command":"bump","args":{{"t":"m","v":{{"by":{{"t":"i","v":"5"}},"path":{{"t":"s","v":"counter"}}}}}}}}"#
            ),
        );
        assert_eq!(
            reply,
            format!(r#"{{"id":"{id}","kind":"returned","value":{{"t":"i","v":"{expected}"}}}}"#),
            "call {id} must be served, not refused as reentrant"
        );
    }

    assert_eq!(
        duet_protocol::handle_text(
            &handle,
            SubscriberId(1),
            r#"{"kind":"get","id":"3","path":"counter"}"#
        ),
        r#"{"id":"3","kind":"value","value":{"t":"i","v":"10"}}"#,
        "the writes the command made must be visible through an ordinary get"
    );
    stop(runtime);
}

#[test]
fn bump_raises_rather_than_refusing_when_the_world_is_wrong() {
    // The other side of the refused/raised line. The call was well-formed; the
    // target simply is not an integer, and that is a state of the store rather
    // than a mistake in the request.
    let (reply, runtime) = serve(
        seeded(),
        r#"{"kind":"invoke","id":"1","command":"bump","args":{"t":"m","v":{"by":{"t":"i","v":"1"},"path":{"t":"s","v":"title"}}}}"#,
    );
    assert_eq!(
        reply,
        r#"{"error":{"t":"m","v":{"code":{"t":"s","v":"not_an_integer"},"found":{"t":"s","v":"string"}}},"id":"1","kind":"raised"}"#
    );

    // ...and an absent path is its own raised code, not the same one.
    let (absent, b) = serve(
        seeded(),
        r#"{"kind":"invoke","id":"2","command":"bump","args":{"t":"m","v":{"by":{"t":"i","v":"1"},"path":{"t":"s","v":"nope"}}}}"#,
    );
    assert_eq!(
        absent,
        r#"{"error":{"t":"m","v":{"code":{"t":"s","v":"absent"}}},"id":"2","kind":"raised"}"#
    );
    stop(runtime);
    stop(b);
}

#[test]
fn bump_refuses_an_unparseable_path_and_bounds_what_it_echoes() {
    // A path is the one argument whose *value* is echoed, because a path is
    // useless to debug unseen. It still has to be bounded.
    let (reply, runtime) = serve(
        seeded(),
        r#"{"kind":"invoke","id":"1","command":"bump","args":{"t":"m","v":{"by":{"t":"i","v":"1"},"path":{"t":"s","v":"a.[0]"}}}}"#,
    );
    let parsed: serde_json::Value = serde_json::from_str(&reply).expect("the reply should be JSON");
    assert_eq!(parsed["kind"], "failed", "{reply}");
    assert!(
        reply.contains("a.[0]"),
        "the refusal must name the path: {reply}"
    );

    let huge = "z".repeat(1_000_000);
    let (bounded, b) = serve(
        seeded(),
        &format!(
            r#"{{"kind":"invoke","id":"2","command":"bump","args":{{"t":"m","v":{{"by":{{"t":"i","v":"1"}},"path":{{"t":"s","v":"a.[0]{huge}"}}}}}}}}"#
        ),
    );
    assert!(
        bounded.len() < 300,
        "a 1 MB path produced a {}-byte reply",
        bounded.len()
    );
    stop(runtime);
    stop(b);
}

#[test]
fn an_unregistered_command_is_failed_and_reveals_nothing_about_the_registry() {
    let (reply, runtime) = serve(
        seeded(),
        r#"{"kind":"invoke","id":"1","command":"subtrac","args":{"t":"m","v":{}}}"#,
    );
    let parsed: serde_json::Value = serde_json::from_str(&reply).expect("the reply should be JSON");
    assert_eq!(parsed["kind"], "failed", "{reply}");
    let message = parsed["message"].as_str().unwrap_or_default();
    assert!(message.contains("subtrac"), "{message}");
    assert!(
        !message.contains("bump") && !message.contains("raise"),
        "a refusal must not enumerate the registry: {message}"
    );
    stop(runtime);
}
