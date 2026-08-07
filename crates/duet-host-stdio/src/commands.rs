//! The commands every session of this host registers.
//!
//! Four of them, hand-written. Three predate any generator, and that is the
//! point: this module is what a guest's `invoke` is measured against *before*
//! one existed, so the generator has something to be compared to rather than
//! being its own specification.
//!
//! # This registry and `schema/app.json` are two statements of one thing
//!
//! The schema declares these four commands, their parameter keys and their
//! result types; this registry is what actually answers them. Nothing in Rust
//! links the two, so `tests/commands.rs` compares them: every command the
//! schema declares must resolve here, and every command registered here must be
//! declared there. A generated client is built from the schema and calls this,
//! so a name in one and not the other is a typed method that cannot be called —
//! with no error until it is.
//!
//! # What each one is for
//!
//! | Command | Proves |
//! |---|---|
//! | [`SUBTRACT`] | arguments arrive under the right names, and a value comes back |
//! | [`RAISE`] | a command that ran and returned `Err` reaches the guest as `raised`, structured |
//! | [`BUMP`] | a command body reads and writes the **same store** the guest reads through |
//! | [`PING`] | a command that declares no result and cannot fail, under a **dotted** name |
//!
//! `subtract` rather than `add` deliberately: subtraction is not commutative,
//! so a guest whose encoder swapped two argument names gets `-7` where it
//! expected `7`. An `add` would answer correctly for a completely broken
//! argument encoder, which is the shape of test this project has been bitten by
//! ten times.
//!
//! # Refused versus raised, as this module draws the line
//!
//! A [`Outcome::Refused`] becomes a `failed`: the host would not run it. Every
//! argument problem is one of these — a missing argument, an argument of the
//! wrong type, a `path` that is not a path — because the call never got as far
//! as doing anything.
//!
//! A [`Outcome::Raised`] becomes a `raised`: the command ran and the answer was
//! a failure. `bump` uses one when the store refused the write or the target
//! held something other than an integer, which is a state of the *world* rather
//! than a mistake in the call.
//!
//! # Nothing here echoes an argument's value
//!
//! Arguments are guest-chosen and unbounded. Every refusal below names the
//! argument and, at most, the *kind* of what arrived — never the thing itself —
//! so a one-megabyte string argument cannot become a one-megabyte reply. The
//! one exception is `path`, which is echoed through [`duet_core::truncated`]
//! because a path is useless to debug without seeing it.

use duet_command::Commands;
use duet_core::{Path, Value};
use duet_protocol::{Args, Outcome};
use duet_runtime::StoreHandle;

/// Returns `a - b`, both `i64` arguments.
pub const SUBTRACT: &str = "subtract";

/// Always returns `Err`, carrying a structured error.
pub const RAISE: &str = "raise";

/// Adds `by` to the integer at `path`, returning the new value.
pub const BUMP: &str = "bump";

/// Answers, and does nothing else.
///
/// The liveness probe a guest uses to prove command RPC is wired end to end,
/// and the one command here whose schema declares neither a `returns` nor a
/// `raises` — the shape a generated client must still be able to call. Its
/// **dotted** name is equally deliberate: a dot is legal in a command name and
/// has to survive to the wire uncamel-cased, which is only checkable if some
/// command actually carries one.
pub const PING: &str = "session.ping";

/// The registry every [`crate::Session`] is built with.
///
/// One registry shape for both schema fixtures. `bump` takes the path to work
/// on as an argument rather than hard-coding one, so the same commands are
/// meaningful whichever fixture seeded the store — and so the "malformed
/// arguments" case has a natural shape to be malformed.
#[must_use]
pub fn commands() -> Commands {
    Commands::new()
        .with(SUBTRACT, subtract)
        .with(RAISE, raise)
        .with(BUMP, bump)
        .with(PING, ping)
}

/// A command that answers and does nothing.
///
/// Returns [`Value::Null`], which is what the wire carries for a command with
/// no result — `schema/app.json` declares no `returns` for it, and the two
/// statements have to agree or a generated client decodes the wrong thing.
fn ping(_args: Args, _store: &StoreHandle) -> Outcome {
    Outcome::Returned(Value::Null)
}

/// `subtract(a, b) -> a - b`.
///
/// Saturating, so a body cannot panic on overflow in a release build and
/// silently wrap in a debug one. `i64::MIN - 1` is not what this exists to
/// explore.
fn subtract(args: Args, _store: &StoreHandle) -> Outcome {
    let a = match int_argument(&args, "a") {
        Ok(a) => a,
        Err(why) => return Outcome::Refused(why),
    };
    let b = match int_argument(&args, "b") {
        Ok(b) => b,
        Err(why) => return Outcome::Refused(why),
    };
    Outcome::Returned(Value::Int(a.saturating_sub(b)))
}

/// The exact error [`RAISE`] returns, as a guest must decode it.
///
/// A map with a string and an integer in it, not a bare string: the whole claim
/// `raised` makes over `failed` is that a typed error survives as something a
/// guest can match on, and a single string would be indistinguishable from the
/// prose a `failed` already carries.
fn raised_error() -> Value {
    Value::map([
        ("code", Value::Str("unlucky".into())),
        ("short_by", Value::Int(42)),
    ])
}

/// A command that ran and returned `Err`.
///
/// Takes no arguments and ignores any it is given: a guest asserting on the
/// exact error payload needs it to be the same payload every time.
fn raise(_args: Args, _store: &StoreHandle) -> Outcome {
    Outcome::Raised(raised_error())
}

/// `bump(path, by) -> the new value at path`.
///
/// The command that matters most here. It reads the store and writes it back
/// through the [`StoreHandle`] it is handed — which works only because a
/// handler runs on the thread that called `dispatch_with` and never on the
/// runtime's core thread. A guest reading `path` afterwards through the ordinary
/// typed path sees the write, which is the whole "commands and state share one
/// world" claim reduced to something a test can assert.
fn bump(args: Args, store: &StoreHandle) -> Outcome {
    let by = match int_argument(&args, "by") {
        Ok(by) => by,
        Err(why) => return Outcome::Refused(why),
    };
    let raw = match args.get("path") {
        Some(Value::Str(s)) => s,
        Some(other) => {
            return Outcome::Refused(format!(
                "argument \"path\" must be a string, got a {}",
                kind_of(other)
            ));
        }
        None => return Outcome::Refused("argument \"path\" is missing".to_string()),
    };
    // Bounded: `path` is guest text, and this is the one place a value of it is
    // echoed at all.
    let path = match Path::parse(raw) {
        Ok(path) => path,
        Err(e) => {
            return Outcome::Refused(format!(
                "argument \"path\" is not a path: {} ({e})",
                duet_core::truncated(&raw)
            ));
        }
    };

    // From here on the call itself was well-formed, so every failure is a
    // `raised`: the command ran, and the world would not let it finish.
    let current = match store.get(&path) {
        Ok(Some(Value::Int(n))) => n,
        Ok(Some(other)) => {
            return Outcome::Raised(Value::map([
                ("code", Value::Str("not_an_integer".into())),
                ("found", Value::Str(kind_of(&other).into())),
            ]));
        }
        Ok(None) => {
            return Outcome::Raised(Value::map([("code", Value::Str("absent".into()))]));
        }
        Err(e) => {
            return Outcome::Raised(Value::map([
                ("code", Value::Str("store".into())),
                ("detail", Value::Str(e.to_string())),
            ]));
        }
    };

    let next = current.saturating_add(by);
    match store.set(&path, Value::Int(next)) {
        Ok(()) => Outcome::Returned(Value::Int(next)),
        Err(e) => Outcome::Raised(Value::map([
            ("code", Value::Str("store".into())),
            ("detail", Value::Str(e.to_string())),
        ])),
    }
}

/// Reads a required `i64` argument.
///
/// # Errors
///
/// The refusal text, naming the argument and the kind that arrived. Never the
/// value: see this module's documentation.
fn int_argument(args: &Args, name: &str) -> Result<i64, String> {
    match args.get(name) {
        Some(Value::Int(n)) => Ok(*n),
        Some(other) => Err(format!(
            "argument \"{name}\" must be an integer, got a {}",
            kind_of(other)
        )),
        None => Err(format!("argument \"{name}\" is missing")),
    }
}

/// Names a value's kind, mirroring `duet_protocol`'s own vocabulary.
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

#[cfg(test)]
#[path = "commands_tests.rs"]
mod tests;
