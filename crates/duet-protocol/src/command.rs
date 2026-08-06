//! Running host commands on a guest's behalf.
//!
//! Reading and writing state is not enough on its own: some things a guest
//! needs done are *logic* — validate this, then write three paths atomically;
//! call a platform API the renderer cannot reach. [`Request::Invoke`] is how a
//! guest asks for that, and this module is the seam the host answers it
//! through.
//!
//! [`Request::Invoke`]: crate::Request::Invoke
//!
//! # Authorization is by construction, not by claim
//!
//! An `invoke` carries no caller identity. A surface reaches exactly the
//! commands in the [`CommandHost`] it was built with, which the *embedder*
//! chose — so a webview running untrusted content simply has no name for a
//! command its own host does not hold. There is nothing for a guest to spoof,
//! because there is nothing it is asked to assert.
//!
//! # What this module guarantees about a command body
//!
//! A body is arbitrary user code. It can panic, it can loop, it can return a
//! `Value` no wire could carry. `run` is the single choke point every
//! `invoke` passes through, so the guarantees are enforced here rather than
//! asked of each implementor:
//!
//! - **A panic becomes a `failed` reply.** Not a process abort, and not an
//!   unanswered guest call.
//! - **An over-deep return becomes a `failed` reply.** And the offending value
//!   is taken apart iteratively before it is dropped, because dropping it is
//!   itself recursive.
//!
//! What is *not* guaranteed, because it cannot be: a body that never returns
//! never returns. See [`CommandHost::invoke`] for which thread that blocks.

use std::panic::{self, AssertUnwindSafe};

use duet_core::Value;
use duet_runtime::StoreHandle;

use crate::message::{Args, RequestId, Response};

/// What running one command produced.
///
/// Three outcomes, not two, and the third is the one worth stating: a command
/// that *ran* and returned an error ([`Outcome::Raised`]) is a different event
/// from one the host would not run at all ([`Outcome::Refused`]). A guest that
/// cannot tell them apart cannot decide whether retrying is safe.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Outcome {
    /// The command returned `Ok(T)`. Becomes [`Response::Returned`].
    ///
    /// A command with no result returns [`Value::Null`], which encodes as the
    /// tagged `{"t":"n"}`.
    Returned(Value),
    /// The command returned `Err(E)`. Becomes [`Response::Raised`].
    ///
    /// A **domain** outcome, carried as a value so the guest can decode it back
    /// into its own error type. Do not use this for "I could not run that" —
    /// that is [`Outcome::Refused`].
    Raised(Value),
    /// The host would not run the command. Becomes [`Response::Failed`].
    ///
    /// Unknown command name, arguments that did not decode, a precondition the
    /// caller could not have known about.
    ///
    /// # The message must already be bounded
    ///
    /// It goes to the guest verbatim. If any part of it is derived from
    /// guest-supplied text — the command name above all — bound that part with
    /// [`duet_core::truncated`] first, or a guest turns a 1 MB command name
    /// into a 1 MB reply. This is stated as an obligation rather than enforced
    /// here because the surrounding prose is host-authored and truncating the
    /// whole message would cut a legitimate explanation short; it is the same
    /// rule `duet_core::SetError` and `duet_codec::CodecError` already follow,
    /// bounding at the point the untrusted fragment enters.
    Refused(String),
}

/// The set of commands one surface may reach.
///
/// # Which thread a handler runs on
///
/// [`invoke`](CommandHost::invoke) runs **synchronously, on the thread that
/// called [`crate::dispatch_with`]**, inside that call's stack frame. In Duet's
/// shipped macOS backend that is the **platform (main) thread**: `wry`'s IPC
/// handler and Flutter's binary-messenger handler are both invoked there, and
/// both call `dispatch_with` inline.
///
/// It is **never** the runtime's core thread. That is the whole reason a
/// handler may use the [`StoreHandle`] it is given:
///
/// | A handler may | A handler must not |
/// |---|---|
/// | `get` / `set` / `subscribe` through the `StoreHandle` | block, sleep, or wait on I/O |
/// | compute for microseconds | wait for anything the main thread must produce |
/// | return a `Value` of any shape within [`duet_core::MAX_VALUE_DEPTH`] | assume it runs on a thread of its own |
///
/// Blocking is the prohibition that matters. The platform thread also drives
/// the UI, so a handler that waits freezes the window it was called from — and
/// Duet has no async runtime to hand the work to instead. There is no tokio
/// anywhere in this workspace, deliberately: an embedder that needs a slow
/// command should spawn its own thread, return immediately, and publish the
/// result into the store when it arrives, where a subscription will deliver it.
///
/// # The one hazard the runtime's own guard cannot see
///
/// `duet_runtime`'s `ON_CORE_THREAD` check refuses a `StoreHandle` call made
/// *from* the core thread, turning what would be a permanent deadlock into an
/// immediate `RuntimeError::ReentrantCall`. It is a **same-thread** check, so:
///
/// - An embedder that calls `dispatch_with` from inside a `Sink::deliver` — which
///   does run on the core thread — gets that error from every `StoreHandle` call
///   in the handler. Ugly, but reported rather than hung.
/// - A cycle through *two* threads is invisible to it. A handler that blocks on
///   the main thread waiting for work only the main thread can do will hang, and
///   nothing in this crate can detect it. Hence: do not block.
///
/// The ordinary case — a handler that reads state, computes, and writes state —
/// is exactly what this is designed for, and
/// `a_command_body_may_call_back_into_the_store` in this module proves it.
pub trait CommandHost {
    /// Runs `command` with `args`, against the shared store.
    ///
    /// Never returns a `Result`: every outcome, including refusal, is an
    /// [`Outcome`], so a host cannot accidentally collapse "the command failed"
    /// and "there is no such command" into one shape.
    ///
    /// `store` is the same handle [`crate::dispatch_with`] was given, passed
    /// through so an implementation need not capture one of its own. See this
    /// trait's docs for what a handler may do with it.
    fn invoke(&self, command: &str, args: Args, store: &StoreHandle) -> Outcome;
}

/// A [`CommandHost`] that refuses every command.
///
/// What [`crate::dispatch()`] and [`crate::handle_text()`] use — the entry points
/// that predate command RPC and take no command host. A host that registers no
/// commands therefore answers an `invoke` with a `failed` naming the command,
/// rather than with an unknown-kind error that would tell a developer their
/// message was malformed when it was perfectly well-formed and simply
/// unserved.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoCommands;

impl CommandHost for NoCommands {
    fn invoke(&self, command: &str, _args: Args, _store: &StoreHandle) -> Outcome {
        Outcome::Refused(format!(
            "this host registers no commands, so there is nothing named \"{}\" to run",
            duet_core::truncated(&command)
        ))
    }
}

/// Runs one command and turns its outcome into a [`Response`].
///
/// The single choke point for every `invoke`, which is what lets the panic
/// guard and the depth guard be unconditional rather than a rule each
/// [`CommandHost`] implementation has to remember.
pub(crate) fn run(
    commands: &dyn CommandHost,
    store: &StoreHandle,
    id: RequestId,
    command: &str,
    args: Args,
) -> Response {
    // `AssertUnwindSafe` is required because `&dyn CommandHost` and
    // `&StoreHandle` are not `UnwindSafe`, and it is sound here rather than
    // merely convenient. What unwind safety guards against is observing a value
    // left half-updated by a panic. Neither of these can be: a `StoreHandle` is
    // a channel sender to another thread, and the store on the far side applies
    // each write whole or not at all, so a body that panicked between two
    // `set`s leaves the store in a state some *other* caller could equally have
    // produced. There is no cross-call invariant here for a panic to break.
    let outcome = panic::catch_unwind(AssertUnwindSafe(|| commands.invoke(command, args, store)));
    match outcome {
        Ok(Outcome::Returned(value)) => match encodable(value) {
            Some(value) => Response::Returned { id, value },
            None => Response::Failed {
                id,
                message: too_deep(command),
            },
        },
        Ok(Outcome::Raised(error)) => match encodable(error) {
            Some(error) => Response::Raised { id, error },
            None => Response::Failed {
                id,
                message: too_deep(command),
            },
        },
        Ok(Outcome::Refused(why)) => Response::Failed { id, message: why },
        // The caught payload is deliberately not included. It is arbitrary user
        // text of arbitrary length — a guest can do nothing with it, a log can
        // be flooded by it, and rendering it is work on a path that exists
        // because work already failed once. Rust has already printed the panic
        // and its backtrace to stderr, which is where a developer looks.
        Err(_) => Response::Failed {
            id,
            message: format!(
                "the command \"{}\" panicked; the host caught it and refused the call",
                duet_core::truncated(&command)
            ),
        },
    }
}

/// Returns `value` if it is shallow enough for any envelope to carry, or `None`
/// after taking it apart if it is not.
///
/// # Why a command's return needs its own guard
///
/// [`duet_core::Store::set`] already refuses to *store* a value this deep, and
/// [`crate::handle_text`] already refuses to *emit* text this deep. A command
/// return slips between the two: it is a `Value` the host constructs, never
/// written to the store, on its way to a reply. Without this check a command
/// could return text the host itself could not re-parse — which is the exact
/// bug `MAX_VALUE_DEPTH` was introduced to close, arriving through a new door.
///
/// The bound is [`duet_core::MAX_VALUE_DEPTH`] rather than the slightly larger
/// depth a `returned` envelope could technically carry. A returned value is
/// very often written straight back into the store by the caller, and a value
/// that encodes today but cannot be stored tomorrow is a worse contract than
/// one number that holds everywhere.
fn encodable(value: Value) -> Option<Value> {
    // `Value::depth` is iterative, so measuring a pathological value is safe
    // even though *holding* one is not.
    if value.depth() <= duet_core::MAX_VALUE_DEPTH {
        return Some(value);
    }
    dismantle(value);
    None
}

/// Drops a [`Value`] iteratively.
///
/// **Not a micro-optimization — the alternative aborts the process.** `Value`
/// has a derived `Drop`, and a derived `Drop` is recursive: letting a
/// 100 000-deep value fall out of scope overflows the stack, and a Rust stack
/// overflow is an abort, not a catchable error. Measured in `duet-core`'s own
/// tests at 20 000 levels.
///
/// Everywhere else in Duet that hazard is out of reach, because nothing deeper
/// than [`duet_core::MAX_VALUE_DEPTH`] can enter the store. A command return is
/// the first value the host holds that never passed through the store — so this
/// is the first place the recursive drop is actually reachable, and rejecting
/// such a value would be pointless if the rejection itself crashed on the way
/// out.
///
/// The explicit stack holds at most one entry per node, which the caller has
/// already paid for by constructing the value.
fn dismantle(value: Value) {
    let mut pending = vec![value];
    while let Some(node) = pending.pop() {
        match node {
            // Each child is *moved* onto the stack, so the container that held
            // them is empty by the time it drops — one level, never a chain.
            Value::List(items) => pending.extend(items),
            Value::Map(entries) => pending.extend(entries.into_values()),
            // Scalars own no `Value`, so dropping one recurses into nothing.
            Value::Null
            | Value::Bool(_)
            | Value::Int(_)
            | Value::Float(_)
            | Value::Str(_)
            | Value::Bytes(_) => {}
        }
    }
}

/// The refusal for a return no envelope can carry.
fn too_deep(command: &str) -> String {
    format!(
        "the command \"{}\" returned a value nesting deeper than the {} containers \
         this wire admits",
        duet_core::truncated(&command),
        duet_core::MAX_VALUE_DEPTH
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use duet_core::{Path, SubscriberId};
    use duet_runtime::{NullSink, Runtime};

    fn rt() -> Runtime {
        Runtime::spawn(Value::map([("count", Value::Int(1))]), NullSink)
    }

    fn p(s: &str) -> Path {
        Path::parse(s).expect("test path should parse")
    }

    /// A `CommandHost` built from one closure, so each test states only the
    /// body it cares about.
    struct OneCommand<F>(F);

    impl<F: Fn(&str, Args, &StoreHandle) -> Outcome> CommandHost for OneCommand<F> {
        fn invoke(&self, command: &str, args: Args, store: &StoreHandle) -> Outcome {
            (self.0)(command, args, store)
        }
    }

    /// Wraps a closure as a [`OneCommand`].
    ///
    /// A constructor function rather than the tuple-struct constructor, because
    /// the bound is what makes the closure's `&str` and `&StoreHandle`
    /// parameters *higher-ranked*. Written as the bare tuple constructor, the
    /// compiler
    /// infers one concrete lifetime from the first call site and then rejects
    /// every later one with "implementation of `Fn` is not general enough".
    fn one<F: Fn(&str, Args, &StoreHandle) -> Outcome>(f: F) -> OneCommand<F> {
        OneCommand(f)
    }

    /// A chain of `n` nested lists around a scalar leaf: `Value::depth() == n`.
    fn chain(n: usize) -> Value {
        let mut v = Value::Int(1);
        for _ in 0..n {
            v = Value::List(vec![v]);
        }
        v
    }

    #[test]
    fn a_returning_command_answers_returned() {
        let rt = rt();
        let host = one(|_, _, _: &StoreHandle| Outcome::Returned(Value::Int(5)));
        assert_eq!(
            run(&host, &rt.handle(), RequestId(1), "add", Args::new()),
            Response::Returned {
                id: RequestId(1),
                value: Value::Int(5)
            }
        );
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn a_raising_command_answers_raised_and_keeps_the_error_typed() {
        let rt = rt();
        let host = one(|_, _, _: &StoreHandle| {
            Outcome::Raised(Value::map([("code", Value::Str("store".into()))]))
        });
        match run(&host, &rt.handle(), RequestId(2), "save", Args::new()) {
            Response::Raised { id, error } => {
                assert_eq!(id, RequestId(2));
                assert_eq!(error, Value::map([("code", Value::Str("store".into()))]));
            }
            other => panic!("expected Raised, got {other:?}"),
        }
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn a_refusal_answers_failed_not_raised() {
        // The distinction the whole `raised`/`failed` split exists for. A host
        // that collapsed them would pass every other test in this file.
        let rt = rt();
        let host = one(|_, _, _: &StoreHandle| Outcome::Refused("no such command".into()));
        assert_eq!(
            run(&host, &rt.handle(), RequestId(3), "nope", Args::new()),
            Response::Failed {
                id: RequestId(3),
                message: "no such command".to_string()
            }
        );
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn a_panicking_command_body_answers_failed_and_names_the_command() {
        // Rust prints the panic to stderr as it unwinds — that output is
        // expected noise from this test, not a sign anything is wrong.
        let rt = rt();
        let host = one(|_, _, _: &StoreHandle| -> Outcome {
            panic!("intentional panic: standing in for a broken command body");
        });
        match run(&host, &rt.handle(), RequestId(4), "boom", Args::new()) {
            Response::Failed { id, message } => {
                assert_eq!(id, RequestId(4), "the guest must be able to fail that call");
                assert!(message.contains("boom"), "got {message}");
                assert!(message.contains("panicked"), "got {message}");
                assert!(
                    !message.contains("intentional panic"),
                    "the caught payload must not be echoed to the guest, got {message}"
                );
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        // The positive control: the host is still serving afterwards. A guard
        // that answered correctly and then left the process wedged would be no
        // guard at all.
        let host = one(|_, _, _: &StoreHandle| Outcome::Returned(Value::Int(1)));
        assert!(matches!(
            run(&host, &rt.handle(), RequestId(5), "ok", Args::new()),
            Response::Returned { .. }
        ));
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn a_panicking_command_name_is_bounded_in_the_refusal() {
        let rt = rt();
        let host = one(|_, _, _: &StoreHandle| -> Outcome {
            panic!("intentional panic: proving the refusal stays bounded");
        });
        let huge = "c".repeat(1_000_000);
        match run(&host, &rt.handle(), RequestId(6), &huge, Args::new()) {
            Response::Failed { message, .. } => assert!(
                message.len() < 300,
                "a 1 MB command name produced a {}-char refusal",
                message.len()
            ),
            other => panic!("expected Failed, got {other:?}"),
        }
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn a_return_at_the_bound_survives_and_one_past_it_is_refused() {
        // The boundary, both sides, because an off-by-one here silently costs a
        // legitimate return one level of nesting.
        let rt = rt();

        let host =
            one(|_, _, _: &StoreHandle| Outcome::Returned(chain(duet_core::MAX_VALUE_DEPTH)));
        assert!(
            matches!(
                run(&host, &rt.handle(), RequestId(7), "deep", Args::new()),
                Response::Returned { .. }
            ),
            "a return at exactly the bound must be delivered"
        );

        let host =
            one(|_, _, _: &StoreHandle| Outcome::Returned(chain(duet_core::MAX_VALUE_DEPTH + 1)));
        match run(&host, &rt.handle(), RequestId(8), "deeper", Args::new()) {
            Response::Failed { id, message } => {
                assert_eq!(id, RequestId(8));
                assert!(
                    message.contains(&duet_core::MAX_VALUE_DEPTH.to_string()),
                    "the refusal must name the limit it enforced, got {message}"
                );
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn a_raised_error_is_depth_guarded_too() {
        // `raised` carries a `Value` on exactly the same wire as `returned`. A
        // guard on only one of them leaves the other able to emit text the host
        // cannot re-parse.
        let rt = rt();
        let host =
            one(|_, _, _: &StoreHandle| Outcome::Raised(chain(duet_core::MAX_VALUE_DEPTH + 1)));
        assert!(
            matches!(
                run(&host, &rt.handle(), RequestId(9), "deep_error", Args::new()),
                Response::Failed { .. }
            ),
            "an over-deep raised error must be refused, not emitted"
        );
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn refusing_a_pathologically_deep_return_does_not_abort_the_process() {
        // The half of the depth guard that is easy to get wrong: rejecting the
        // value is not enough if *dropping* it takes the process down on the
        // way out. `Value`'s derived `Drop` is recursive and 100 000 levels
        // overflows a thread's stack — measured in `duet-core`, which resorts
        // to `mem::forget` to survive its own test. This one does not leak; it
        // dismantles, which is what `run` does in production.
        //
        // If `dismantle` were removed, this test aborts the whole test binary
        // rather than failing — so an abort here is the signal, not a flake.
        let rt = rt();
        let host = one(|_, _, _: &StoreHandle| Outcome::Returned(chain(100_000)));
        assert!(matches!(
            run(
                &host,
                &rt.handle(),
                RequestId(10),
                "pathological",
                Args::new()
            ),
            Response::Failed { .. }
        ));
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn dismantle_frees_a_wide_and_deep_tree_without_recursing() {
        // `dismantle` must handle maps as well as lists, and branching as well
        // as chains — a value returned by a real command is a struct, not a
        // linked list. Reached directly because `run` only calls it for values
        // it is refusing.
        let mut deep = Value::map([("leaf", Value::Int(1))]);
        for i in 0..50_000 {
            deep = Value::Map(
                [
                    (format!("child{i}"), deep),
                    ("sibling".to_string(), Value::Str("x".into())),
                ]
                .into_iter()
                .collect(),
            );
        }
        deep = Value::List(vec![deep, Value::Bytes(vec![1, 2, 3])]);
        dismantle(deep);
    }

    #[test]
    fn a_command_body_may_call_back_into_the_store() {
        // THE reentrancy test, and the desirable case rather than the
        // pathological one: read state, compute, write state — which is most of
        // what a command is for.
        //
        // `duet_runtime`'s `ON_CORE_THREAD` guard refuses a `StoreHandle` call
        // made from the core thread. A command handler does NOT run there: it
        // runs on whichever thread called `dispatch_with`, which for both macOS
        // surfaces is the platform thread. So these calls must simply work, and
        // the assertion is on the returned value AND on what landed in the
        // store — a body whose `set` silently failed would still return 2.
        let rt = rt();
        let handle = rt.handle();
        let host = one(|_, _, store: &StoreHandle| {
            let path = Path::parse("count").expect("test path should parse");
            let current = match store.get(&path) {
                Ok(Some(Value::Int(n))) => n,
                other => return Outcome::Refused(format!("unreadable count: {other:?}")),
            };
            let next = current + 1;
            match store.set(&path, Value::Int(next)) {
                Ok(()) => Outcome::Returned(Value::Int(next)),
                Err(e) => Outcome::Raised(Value::Str(e.to_string())),
            }
        });

        assert_eq!(
            run(&host, &handle, RequestId(11), "increment", Args::new()),
            Response::Returned {
                id: RequestId(11),
                value: Value::Int(2)
            },
            "a body that reads and writes the store must succeed, not report ReentrantCall"
        );
        assert_eq!(
            handle.get(&p("count")).expect("read should succeed"),
            Some(Value::Int(2)),
            "the body's write must really have landed in the store"
        );

        // Twice, because a guard that let the first call through and wedged the
        // core thread would still pass a single-call assertion.
        assert_eq!(
            run(&host, &handle, RequestId(12), "increment", Args::new()),
            Response::Returned {
                id: RequestId(12),
                value: Value::Int(3)
            }
        );
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn a_command_body_called_on_the_core_thread_is_refused_rather_than_deadlocked() {
        // The mirror of the test above, and the reason the rustdoc names a
        // thread rather than saying "any thread". An embedder that drives
        // `run` from inside a `Sink::deliver` is on the core thread, where a
        // `StoreHandle` call cannot be served — it would block the very thread
        // that must answer it. `ON_CORE_THREAD` turns that permanent hang into
        // an immediate error the body can report.
        use duet_runtime::{Sink, SinkError};
        use std::sync::{Arc, Mutex};

        #[derive(Clone)]
        struct InvokingSink {
            handle: Arc<Mutex<Option<StoreHandle>>>,
            observed: Arc<Mutex<Option<Response>>>,
        }

        impl Sink for InvokingSink {
            fn deliver(&self, _batch: Vec<duet_core::Notification>) -> Result<(), SinkError> {
                let handle = self
                    .handle
                    .lock()
                    .expect("lock should not be poisoned")
                    .clone()
                    .expect("handle should have been set before delivery");
                let host = one(|_: &str, _: Args, store: &StoreHandle| {
                    let path = Path::parse("count").expect("test path should parse");
                    match store.get(&path) {
                        Ok(_) => Outcome::Returned(Value::Bool(true)),
                        Err(e) => Outcome::Raised(Value::Str(e.to_string())),
                    }
                });
                let response = run(&host, &handle, RequestId(13), "read", Args::new());
                *self.observed.lock().expect("lock should not be poisoned") = Some(response);
                Ok(())
            }
        }

        let observed = Arc::new(Mutex::new(None));
        let sink = InvokingSink {
            handle: Arc::new(Mutex::new(None)),
            observed: observed.clone(),
        };
        let rt = Runtime::spawn(Value::map([("count", Value::Int(1))]), sink.clone());
        let handle = rt.handle();
        *sink.handle.lock().expect("lock should not be poisoned") = Some(handle.clone());

        handle
            .subscribe(SubscriberId(1), p("count"))
            .expect("subscribe should succeed");
        handle
            .set(&p("count"), Value::Int(2))
            .expect("the write itself must succeed");
        // The store must still be usable from this (non-core) thread, which it
        // could not be if the nested call had deadlocked the core thread.
        assert_eq!(
            handle.get(&p("count")).expect("read should succeed"),
            Some(Value::Int(2)),
            "the guard must leave the store working, not merely report the problem"
        );
        rt.shutdown().expect("shutdown should succeed");

        match observed
            .lock()
            .expect("lock should not be poisoned")
            .clone()
        {
            Some(Response::Raised { error, .. }) => {
                let rendered = match error {
                    Value::Str(s) => s,
                    other => panic!("expected a string error, got {other:?}"),
                };
                assert!(
                    rendered.contains("deadlock"),
                    "the body must have seen ReentrantCall, got {rendered}"
                );
            }
            other => panic!("expected the body to report a reentrant call, got {other:?}"),
        }
    }

    #[test]
    fn a_host_with_no_commands_refuses_by_name_rather_than_by_shape() {
        // What `dispatch` and `handle_text` do with an `invoke`. The message
        // must say "there is nothing by that name", not "malformed request" —
        // the guest's message was perfectly well-formed and simply unserved,
        // and telling a developer otherwise sends them to debug their encoder.
        let rt = rt();
        match run(&NoCommands, &rt.handle(), RequestId(14), "add", Args::new()) {
            Response::Failed { id, message } => {
                assert_eq!(id, RequestId(14));
                assert!(message.contains("add"), "got {message}");
                assert!(message.contains("no commands"), "got {message}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn no_commands_bounds_the_command_name_it_reports() {
        let rt = rt();
        let huge = "c".repeat(1_000_000);
        match run(&NoCommands, &rt.handle(), RequestId(15), &huge, Args::new()) {
            Response::Failed { message, .. } => assert!(
                message.len() < 300,
                "a 1 MB command name produced a {}-char refusal",
                message.len()
            ),
            other => panic!("expected Failed, got {other:?}"),
        }
        rt.shutdown().expect("shutdown should succeed");
    }
}
