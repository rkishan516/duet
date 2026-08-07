//! `#[command]` re-expressing `duet-host-stdio`'s hand-written registry, and
//! answering byte-for-byte the same.
//!
//! # Why this is the proof and the goldens are not
//!
//! `crates/duet-host-stdio/src/commands.rs` was written before any generator
//! existed, precisely so that a later generator would have something to be
//! compared to rather than being its own specification. Its commands are what
//! the Dart and JavaScript live conformance runs already invoke, and their reply
//! bytes are asserted there.
//!
//! So this file re-expresses all of them with `#[command]` and drives **both**
//! registries — the hand-written one, imported from the host crate, and the
//! generated one — through `duet_protocol::handle_text_with` over the same
//! table of requests, on two stores seeded identically. Nothing about the
//! expansion is inspected; only what a guest would receive.
//!
//! # What is identical, and what is not
//!
//! Every reply's **kind** is identical on all fourteen cases: a guest branches
//! the same way whichever registry served it. Every `returned` is byte-for-byte
//! identical. The differences are all in what a *failure* carries, and there are
//! two classes of them. Both are recorded here because both are findings.
//!
//! ## 1. A command has one error type, so `raised` has one shape
//!
//! The hand-written `bump` raises three differently shaped maps —
//! `{code}`, `{code, found}`, `{code, detail}` — under one command name. That
//! has no schema: a `named` type's decode requires every key, so no single
//! `raises` type describes all three, and a generated guest client would have
//! nothing to decode a `raised` through.
//!
//! A `#[command]` therefore raises **one** type, with the facts that do not
//! apply carried as `null`. That is a consequence of commands having a schema
//! at all rather than a limitation of the macro, and it is the direction the
//! whole increment exists to move in — but it is a wire change, and a guest
//! matching on the *absence* of `detail` would see it.
//!
//! ## 2. A body cannot refuse, so one refusal moved and two were reworded
//!
//! `bump` refuses a `path` argument that is not a path — an
//! `Outcome::Refused` produced from **inside the body**, after both arguments
//! decoded. A `#[command]` body cannot produce a refusal: it returns a value or
//! an `Err`, and an `Err` is a `raised`. See `duet_command::CommandReturn` for
//! why that is deliberate rather than missing.
//!
//! The fix is not a new escape hatch, it is a type. `PathArg` below is a
//! hand-written `SharedState` whose `from_value` rejects a string that does not
//! parse, so the refusal happens where `CommandParam` decodes the argument —
//! still a `failed`, still naming `path`, and now never echoing the guest's
//! text at all. The prose differs, and so does the prose on the two
//! wrong-argument-type cases, because `CommandParam` composes those messages
//! once instead of each body composing its own.

use duet::{
    CommandContext, CommandEntry, Commands, DecodeError, NotNullable, Registry, SharedState, Ty,
    Value, command, commands,
};
use duet_core::{Path, SubscriberId};
use duet_protocol::handle_text_with;
use duet_runtime::{NullSink, Runtime};

/// The structured error `raise` returns, as a guest must decode it.
#[derive(SharedState, Debug, PartialEq)]
struct Unlucky {
    code: String,
    short_by: i64,
}

/// The structured error `bump` raises when the world will not let it finish.
#[derive(SharedState, Debug, PartialEq)]
struct BumpError {
    code: String,
    detail: Option<String>,
    found: Option<String>,
}

impl BumpError {
    fn of(code: &str) -> BumpError {
        BumpError {
            code: code.to_string(),
            detail: None,
            found: None,
        }
    }
}

/// A store path, as an argument.
///
/// The hand-written `bump` takes `path: String` and parses it in the body,
/// refusing when it does not parse. A `#[command]` body cannot refuse — so the
/// parse moves into the argument's own decode, where a failure is already a
/// refusal. This is the documented escape hatch (`impl SharedState` by hand)
/// doing exactly what it exists for.
#[derive(Debug, PartialEq)]
struct PathArg(Path);

impl SharedState for PathArg {
    fn to_value(&self) -> Value {
        Value::Str(self.0.to_string())
    }

    fn from_value(value: &Value) -> Result<PathArg, DecodeError> {
        match value {
            Value::Str(text) => Path::parse(text)
                .map(PathArg)
                // The value is never echoed: `path` is guest-chosen text of
                // guest-chosen length.
                .map_err(|_| DecodeError::wrong_type("a store path", value)),
            other => Err(DecodeError::wrong_type("a store path", other)),
        }
    }

    fn schema(_registry: &mut Registry) -> Ty {
        Ty::Str
    }
}

impl NotNullable for PathArg {}

#[command]
fn subtract(a: i64, b: i64) -> i64 {
    a.saturating_sub(b)
}

#[command]
fn raise() -> Result<(), Unlucky> {
    Err(Unlucky {
        code: "unlucky".to_string(),
        short_by: 42,
    })
}

/// `session.ping`: no arguments, no result, no error.
///
/// The one command here that cannot differ from its hand-written twin in any
/// way, which is why it is worth including: a registry comparison that only
/// covered the interesting commands would not notice this one going missing.
#[command(rename = "session.ping")]
fn ping() {}

#[command]
fn bump(ctx: &CommandContext, path: PathArg, by: i64) -> Result<i64, BumpError> {
    let current = match ctx.store().get(&path.0) {
        Ok(Some(Value::Int(n))) => n,
        Ok(Some(other)) => {
            return Err(BumpError {
                found: Some(kind_of(&other).to_string()),
                ..BumpError::of("not_an_integer")
            });
        }
        Ok(None) => return Err(BumpError::of("absent")),
        Err(e) => {
            return Err(BumpError {
                detail: Some(e.to_string()),
                ..BumpError::of("store")
            });
        }
    };
    let next = current.saturating_add(by);
    ctx.store()
        .set(&path.0, Value::Int(next))
        .map_err(|e| BumpError {
            detail: Some(e.to_string()),
            ..BumpError::of("store")
        })?;
    Ok(next)
}

/// Names a value's kind, exactly as the hand-written module does.
fn kind_of(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Int(_) => "int",
        Value::Float(_) => "float",
        Value::Str(_) => "string",
        Value::Bytes(_) => "bytes",
        Value::List(_) => "list",
        Value::Map(_) => "map",
    }
}

static COMMANDS: [CommandEntry; 4] = commands![subtract, raise, bump, ping];

/// The seed both hosts start from — the `app` fixture's shape, reduced to what
/// these three commands touch.
fn seeded() -> Value {
    Value::map([
        ("counter", Value::Int(0)),
        ("title", Value::Str(String::new())),
    ])
}

/// Every request both registries are driven with.
///
/// One per branch the hand-written module documents, so a difference anywhere
/// in either is visible. The `path` cases deliberately include the malformed
/// one that motivates this file's second test.
const REQUESTS: &[&str] = &[
    // subtract: both orderings, and both argument failures.
    r#"{"kind":"invoke","id":"1","command":"subtract","args":{"t":"m","v":{"a":{"t":"i","v":"10"},"b":{"t":"i","v":"3"}}}}"#,
    r#"{"kind":"invoke","id":"2","command":"subtract","args":{"t":"m","v":{"b":{"t":"i","v":"10"},"a":{"t":"i","v":"3"}}}}"#,
    r#"{"kind":"invoke","id":"3","command":"subtract","args":{"t":"m","v":{"a":{"t":"i","v":"1"}}}}"#,
    r#"{"kind":"invoke","id":"4","command":"subtract","args":{"t":"m","v":{"a":{"t":"i","v":"1"},"b":{"t":"s","v":"2"}}}}"#,
    // raise: the structured error, and the promise that arguments are ignored.
    r#"{"kind":"invoke","id":"5","command":"raise","args":{"t":"m","v":{}}}"#,
    r#"{"kind":"invoke","id":"6","command":"raise","args":{"t":"m","v":{"junk":{"t":"i","v":"1"}}}}"#,
    // bump: the happy path, then every way the world says no.
    r#"{"kind":"invoke","id":"7","command":"bump","args":{"t":"m","v":{"path":{"t":"s","v":"counter"},"by":{"t":"i","v":"5"}}}}"#,
    r#"{"kind":"invoke","id":"8","command":"bump","args":{"t":"m","v":{"path":{"t":"s","v":"title"},"by":{"t":"i","v":"1"}}}}"#,
    r#"{"kind":"invoke","id":"9","command":"bump","args":{"t":"m","v":{"path":{"t":"s","v":"absent"},"by":{"t":"i","v":"1"}}}}"#,
    // bump: the argument failures.
    r#"{"kind":"invoke","id":"10","command":"bump","args":{"t":"m","v":{"by":{"t":"i","v":"1"}}}}"#,
    r#"{"kind":"invoke","id":"11","command":"bump","args":{"t":"m","v":{"path":{"t":"i","v":"1"},"by":{"t":"i","v":"1"}}}}"#,
    r#"{"kind":"invoke","id":"12","command":"bump","args":{"t":"m","v":{"path":{"t":"s","v":"counter"}}}}"#,
    // The one that differs: a `path` that is a string and is not a path.
    r#"{"kind":"invoke","id":"13","command":"bump","args":{"t":"m","v":{"path":{"t":"s","v":"a..b"},"by":{"t":"i","v":"1"}}}}"#,
    // An unregistered name, which neither registry should serve.
    r#"{"kind":"invoke","id":"14","command":"nope","args":{"t":"m","v":{}}}"#,
];

/// Drives `commands` through every request against a freshly seeded store.
fn transcript(commands: &Commands) -> Vec<String> {
    let runtime = Runtime::spawn(seeded(), NullSink);
    let replies = REQUESTS
        .iter()
        .map(|request| handle_text_with(&runtime.handle(), SubscriberId(1), commands, request))
        .collect();
    runtime.shutdown().expect("shutdown should succeed");
    replies
}

/// A reply's `kind`, which is the part a guest branches on.
fn kind(reply: &str) -> &str {
    for kind in ["returned", "raised", "failed"] {
        if reply.contains(&format!("\"kind\":\"{kind}\"")) {
            return kind;
        }
    }
    "unknown"
}

#[test]
fn the_generated_registry_registers_the_same_names() {
    assert_eq!(
        Commands::from_entries(&COMMANDS).names(),
        duet_host_stdio::commands().names(),
        "the two registries must expose the same authorization surface"
    );
}

/// Every request whose reply differs, and the recorded reason.
///
/// Keyed by the request's wire id, which is unique in [`REQUESTS`]. A case that
/// stopped differing fails here just as loudly as one that started: this is a
/// measurement of the gap, and a measurement that only tightens silently is not
/// one.
const EXPECTED_DIFFERENCES: &[(&str, &str)] = &[
    (
        "\"id\":\"4\"",
        "`subtract`'s wrong-argument-type refusal: composed by `CommandParam` \
         rather than by the body",
    ),
    (
        "\"id\":\"8\"",
        "`bump` raising `not_an_integer`: one error type, so `detail` is present \
         and null rather than absent",
    ),
    (
        "\"id\":\"9\"",
        "`bump` raising `absent`: same, for both `detail` and `found`",
    ),
    (
        "\"id\":\"11\"",
        "`bump`'s wrong-argument-type refusal for `path`: composed by \
         `CommandParam` rather than by the body",
    ),
    (
        "\"id\":\"13\"",
        "`bump`'s malformed-path refusal: a body cannot refuse, so the parse \
         moved into `PathArg`'s decode — and no longer echoes the path",
    ),
];

#[test]
fn every_reply_kind_agrees_so_a_guest_branches_identically() {
    // The property that actually matters to a guest. A `raised` that became a
    // `failed` would change whether retrying is safe; different prose does not.
    let generated = transcript(&Commands::from_entries(&COMMANDS));
    let handwritten = transcript(&duet_host_stdio::commands());
    assert_eq!(generated.len(), REQUESTS.len());
    for ((request, a), b) in REQUESTS.iter().zip(&generated).zip(&handwritten) {
        assert_eq!(kind(a), kind(b), "for {request}\n  {a}\n  {b}");
        assert_ne!(kind(a), "unknown", "for {request}: {a}");
    }
}

#[test]
fn every_successful_reply_is_byte_identical() {
    // Including `raise`, whose whole point is a structured error surviving as
    // something a guest can match on — and including both orderings of
    // `subtract`'s arguments, which is what makes a swapped binding visible.
    let generated = transcript(&Commands::from_entries(&COMMANDS));
    let handwritten = transcript(&duet_host_stdio::commands());
    for ((request, a), b) in REQUESTS.iter().zip(&generated).zip(&handwritten) {
        if kind(a) == "returned" {
            assert_eq!(a, b, "for {request}");
        }
    }
    assert_eq!(
        generated
            .iter()
            .filter(|reply| kind(reply) == "returned")
            .count(),
        3,
        "three of the fourteen cases succeed; if that changed, the table did"
    );
}

#[test]
fn the_replies_that_differ_are_exactly_the_recorded_ones() {
    let generated = transcript(&Commands::from_entries(&COMMANDS));
    let handwritten = transcript(&duet_host_stdio::commands());

    let differing: Vec<&str> = REQUESTS
        .iter()
        .zip(&generated)
        .zip(&handwritten)
        .filter(|((_, a), b)| a != b)
        .map(|((request, _), _)| *request)
        .collect();
    let expected: Vec<&str> = EXPECTED_DIFFERENCES.iter().map(|(id, _)| *id).collect();

    assert_eq!(
        differing.len(),
        expected.len(),
        "the recorded gap has moved. Differing:\n{}",
        differing.join("\n")
    );
    for (id, why) in EXPECTED_DIFFERENCES {
        assert!(
            differing.iter().any(|request| request.contains(id)),
            "{id} no longer differs, but is recorded as differing because {why}"
        );
    }
}

#[test]
fn a_refusal_that_was_reworded_still_names_the_argument_and_echoes_nothing() {
    // What a reworded refusal must not lose. The prose may change; naming the
    // argument that has to be fixed may not, and neither may the bound on guest
    // text.
    let generated = transcript(&Commands::from_entries(&COMMANDS));
    for (id, argument) in [
        ("\"id\":\"4\"", "b"),
        ("\"id\":\"11\"", "path"),
        ("\"id\":\"13\"", "path"),
    ] {
        let at = REQUESTS
            .iter()
            .position(|request| request.contains(id))
            .unwrap_or_else(|| panic!("{id} is in the table"));
        let reply = &generated[at];
        assert_eq!(kind(reply), "failed", "{id}: {reply}");
        assert!(
            reply.contains(argument),
            "{id} must name {argument}: {reply}"
        );
    }
    let malformed = REQUESTS
        .iter()
        .position(|request| request.contains("a..b"))
        .expect("the malformed-path case is in the table");
    assert!(
        !generated[malformed].contains("a..b"),
        "the generated refusal must not echo guest text: {}",
        generated[malformed]
    );
}

#[test]
fn a_raised_error_still_carries_the_same_code_whichever_registry_served_it() {
    // The part of a `raised` a guest actually branches on. The shape around it
    // changed; the code did not.
    let generated = transcript(&Commands::from_entries(&COMMANDS));
    let handwritten = transcript(&duet_host_stdio::commands());
    for ((request, a), b) in REQUESTS.iter().zip(&generated).zip(&handwritten) {
        if kind(a) != "raised" {
            continue;
        }
        let code = |reply: &str| {
            reply
                .split("\"code\":{\"t\":\"s\",\"v\":\"")
                .nth(1)
                .and_then(|rest| rest.split('"').next())
                .map(str::to_string)
        };
        assert_eq!(code(a), code(b), "for {request}\n  {a}\n  {b}");
        assert!(code(a).is_some(), "for {request}: {a}");
    }
}

#[test]
fn a_refusal_never_grows_with_the_argument_that_caused_it() {
    // The hand-written module bounds `path` with `duet_core::truncated` because
    // it echoes it. The generated one never echoes it at all, which is stronger
    // — but "stronger" is an argument, so it is measured.
    let huge = "x".repeat(1_000_000);
    let request = format!(
        r#"{{"kind":"invoke","id":"1","command":"bump","args":{{"t":"m","v":{{"path":{{"t":"s","v":"{huge}"}},"by":{{"t":"i","v":"1"}}}}}}}}"#
    );
    let runtime = Runtime::spawn(seeded(), NullSink);
    let reply = handle_text_with(
        &runtime.handle(),
        SubscriberId(1),
        &Commands::from_entries(&COMMANDS),
        &request,
    );
    assert!(
        reply.len() < 300,
        "a 1 MB path produced a {}-char reply",
        reply.len()
    );
    runtime.shutdown().expect("shutdown should succeed");
}
