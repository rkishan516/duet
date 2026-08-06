//! A registry of host commands a Duet guest may invoke.
//!
//! `duet-protocol` defines the *seam* — [`CommandHost`], [`Outcome`], [`Args`]
//! — because `duet_protocol::dispatch_with` has to name it. This crate is the
//! other half: [`Commands`], a registry built from closures, which is what an
//! embedder actually writes.
//!
//! ```
//! use duet_command::Commands;
//! use duet_core::Value;
//! use duet_protocol::{Outcome, RequestId, Request, Response, dispatch_with};
//! use duet_runtime::{NullSink, Runtime};
//!
//! let runtime = Runtime::spawn(Value::map([("count", Value::Int(1))]), NullSink);
//! let commands = Commands::new().with("add", |args, _store| {
//!     match args.get("a") {
//!         Some(Value::Int(a)) => Outcome::Returned(Value::Int(a + 3)),
//!         _ => Outcome::Refused("expected an int argument \"a\"".to_string()),
//!     }
//! });
//!
//! let reply = dispatch_with(
//!     &runtime.handle(),
//!     duet_core::SubscriberId(1),
//!     &commands,
//!     Request::Invoke {
//!         id: RequestId(1),
//!         command: "add".to_string(),
//!         args: duet_protocol::Args::from([("a".to_string(), Value::Int(2))]),
//!     },
//! );
//! assert_eq!(reply, Response::Returned { id: RequestId(1), value: Value::Int(5) });
//! runtime.shutdown()?;
//! # Ok::<(), duet_runtime::RuntimeError>(())
//! ```
//!
//! # What a handler may do, and on which thread
//!
//! Every word of [`duet_protocol::CommandHost`]'s contract applies here
//! unchanged: a handler runs synchronously on the thread that called
//! `dispatch_with` — the platform thread, in Duet's macOS backend — it may use
//! the [`StoreHandle`] it is given freely, and it **must not block**. Read that
//! trait's docs before writing a handler; they are not repeated here, because a
//! second copy of a threading contract is a second thing to get wrong.
//!
//! # What this crate does not do
//!
//! It does not catch panics and it does not bound the depth of what a handler
//! returns. Both are handled at `duet_protocol::dispatch_with`, which is the
//! single choke point *every* `CommandHost` passes through — including
//! hand-written ones this crate never sees. Duplicating the guards here would
//! make them look optional.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

use std::collections::BTreeMap;

use duet_protocol::{Args, CommandHost, Outcome};
use duet_runtime::StoreHandle;

/// One command's body.
///
/// `Send + Sync` so a [`Commands`] can be shared between surfaces — two guests
/// over one store is the normal arrangement, and a registry that could not
/// cross a thread would force the embedder to build two. A closure capturing
/// nothing (the usual shape, since the [`StoreHandle`] is passed in) satisfies
/// both automatically.
type Handler = Box<dyn Fn(Args, &StoreHandle) -> Outcome + Send + Sync>;

/// The commands one surface may reach, by name.
///
/// # This *is* the authorization boundary
///
/// A guest names a command; it does not name a permission, a role, or itself.
/// Whether the name resolves is decided entirely by what the embedder put in
/// the `Commands` it built the surface with — so a webview running untrusted
/// content and a trusted Flutter surface can be given two different registries
/// over the same store, and the webview has no vocabulary for the commands it
/// was not given. See [`duet_protocol::dispatch_with`].
///
/// A `BTreeMap` rather than a `HashMap`: lookup is not a hot path (one per
/// `invoke`, against a table of tens of entries), and deterministic iteration
/// keeps [`Commands::names`] stable for diagnostics and tests.
#[derive(Default)]
pub struct Commands {
    table: BTreeMap<String, Handler>,
}

impl Commands {
    /// An empty registry, which refuses every command.
    pub fn new() -> Self {
        Commands {
            table: BTreeMap::new(),
        }
    }

    /// Registers `handler` under `name`, returning the registry.
    ///
    /// Consuming and returning `self` makes a registry an expression rather
    /// than a sequence of statements against a `mut` binding, which is what
    /// lets the whole authorization surface of a program be read in one place:
    ///
    /// ```
    /// # use duet_command::Commands;
    /// # use duet_core::Value;
    /// # use duet_protocol::Outcome;
    /// let commands = Commands::new()
    ///     .with("documents.rename", |_args, _store| Outcome::Returned(Value::Null))
    ///     .with("documents.close", |_args, _store| Outcome::Returned(Value::Null));
    /// assert_eq!(commands.names(), ["documents.close", "documents.rename"]);
    /// ```
    ///
    /// # A repeated name replaces, and does not merge
    ///
    /// Plain `BTreeMap` insertion: the last registration for a name wins. Not
    /// special-cased, and deliberately not an error — the alternative is a
    /// fallible builder on a path that runs once at startup, where the mistake
    /// it would catch (registering one name twice) is visible in the same
    /// expression. `names()` shows what was actually registered.
    #[must_use]
    pub fn with(
        mut self,
        name: impl Into<String>,
        handler: impl Fn(Args, &StoreHandle) -> Outcome + Send + Sync + 'static,
    ) -> Self {
        self.table.insert(name.into(), Box::new(handler));
        self
    }

    /// Every registered name, in sorted order.
    ///
    /// For diagnostics and tests. Deliberately **not** exposed to guests: a
    /// guest that could enumerate the commands it may reach would learn the
    /// shape of the host's API, and a guest that cannot enumerate them learns
    /// nothing from a refusal beyond "not that one".
    pub fn names(&self) -> Vec<&str> {
        self.table.keys().map(String::as_str).collect()
    }

    /// How many commands are registered.
    pub fn len(&self) -> usize {
        self.table.len()
    }

    /// Whether no commands are registered.
    pub fn is_empty(&self) -> bool {
        self.table.is_empty()
    }
}

impl std::fmt::Debug for Commands {
    /// Prints the registered names, never the handlers.
    ///
    /// A `Box<dyn Fn>` has no useful `Debug`, and the names are the only part
    /// anyone wants to see when a command did not resolve.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Commands")
            .field("names", &self.names())
            .finish()
    }
}

impl CommandHost for Commands {
    /// Looks `command` up and runs it.
    ///
    /// An unregistered name is an [`Outcome::Refused`] — the host would not run
    /// it — never an [`Outcome::Raised`], which would tell a guest that
    /// something ran and failed.
    ///
    /// The name is bounded with [`duet_core::truncated`] before it reaches the
    /// message. `command` is guest-chosen text of guest-chosen length, and this
    /// is the one place in this crate where it is echoed at all; without the
    /// bound, a 1 MB command name becomes a 1 MB reply and a 1 MB log line.
    fn invoke(&self, command: &str, args: Args, store: &StoreHandle) -> Outcome {
        match self.table.get(command) {
            Some(handler) => handler(args, store),
            // The refusal names the command and nothing else — not the
            // registered names, not a near-match suggestion. A guest learns
            // only that this one name did not resolve, which is all it needs
            // and all it should have.
            None => Outcome::Refused(format!(
                "no command named \"{}\" is registered for this surface",
                duet_core::truncated(&command)
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use duet_core::{Path, SubscriberId, Value};
    use duet_protocol::{Request, RequestId, Response, dispatch_with, handle_text_with};
    use duet_runtime::{NullSink, Runtime};

    fn rt() -> Runtime {
        Runtime::spawn(Value::map([("count", Value::Int(1))]), NullSink)
    }

    fn p(s: &str) -> Path {
        Path::parse(s).expect("test path should parse")
    }

    fn invoke(id: u64, command: &str, args: Args) -> Request {
        Request::Invoke {
            id: RequestId(id),
            command: command.to_string(),
            args,
        }
    }

    #[test]
    fn a_registered_command_runs_and_returns() {
        let rt = rt();
        let commands =
            Commands::new().with("add", |args: Args, _: &StoreHandle| match args.get("a") {
                Some(Value::Int(a)) => Outcome::Returned(Value::Int(a + 3)),
                _ => Outcome::Refused("expected an int argument \"a\"".to_string()),
            });
        assert_eq!(
            dispatch_with(
                &rt.handle(),
                SubscriberId(1),
                &commands,
                invoke(1, "add", Args::from([("a".to_string(), Value::Int(2))])),
            ),
            Response::Returned {
                id: RequestId(1),
                value: Value::Int(5)
            }
        );
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn an_unregistered_name_is_refused_and_reveals_nothing_about_what_is_registered() {
        // Two properties. The refusal is a `failed` (the host would not run it)
        // rather than a `raised` (something ran and failed). And it names only
        // the command that was asked for — a message that helpfully listed the
        // registered names, or suggested a near match, would hand a guest a map
        // of the host's API one typo at a time.
        let rt = rt();
        let commands = Commands::new()
            .with("documents.rename", |_, _: &StoreHandle| {
                Outcome::Returned(Value::Null)
            })
            .with("secret.rotate_keys", |_, _: &StoreHandle| {
                Outcome::Returned(Value::Null)
            });
        match dispatch_with(
            &rt.handle(),
            SubscriberId(1),
            &commands,
            invoke(2, "secret.rotate_key", Args::new()),
        ) {
            Response::Failed { id, message } => {
                assert_eq!(id, RequestId(2));
                assert!(message.contains("secret.rotate_key"), "got {message}");
                assert!(
                    !message.contains("documents.rename"),
                    "the refusal must not enumerate the registry, got {message}"
                );
                assert!(
                    !message.contains("secret.rotate_keys"),
                    "the refusal must not suggest near matches, got {message}"
                );
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn an_empty_registry_refuses_everything() {
        let rt = rt();
        let commands = Commands::new();
        assert!(commands.is_empty());
        assert_eq!(commands.len(), 0);
        assert!(commands.names().is_empty());
        assert!(matches!(
            dispatch_with(
                &rt.handle(),
                SubscriberId(1),
                &commands,
                invoke(3, "anything", Args::new()),
            ),
            Response::Failed { .. }
        ));
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn a_1_mb_command_name_produces_a_small_refusal() {
        // `command` is guest-chosen text on the path this crate echoes it. The
        // reply must not scale with it.
        let rt = rt();
        let commands =
            Commands::new().with("add", |_, _: &StoreHandle| Outcome::Returned(Value::Null));
        match dispatch_with(
            &rt.handle(),
            SubscriberId(1),
            &commands,
            invoke(4, &"c".repeat(1_000_000), Args::new()),
        ) {
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
    fn a_command_body_reads_and_writes_the_store_it_is_handed() {
        // The reentrancy case, through the registry an embedder actually
        // writes: a handler is called on the caller's thread — never the
        // runtime's core thread — so `StoreHandle` works normally inside it.
        // Asserted on the store afterwards as well as on the reply, because a
        // body whose `set` silently failed would still return the right number.
        let rt = rt();
        let handle = rt.handle();
        let commands = Commands::new().with("increment", |_, store: &StoreHandle| {
            let path = Path::parse("count").expect("test path should parse");
            let current = match store.get(&path) {
                Ok(Some(Value::Int(n))) => n,
                other => return Outcome::Raised(Value::Str(format!("unreadable: {other:?}"))),
            };
            match store.set(&path, Value::Int(current + 1)) {
                Ok(()) => Outcome::Returned(Value::Int(current + 1)),
                Err(e) => Outcome::Raised(Value::Str(e.to_string())),
            }
        });

        for (id, expected) in [(5u64, 2i64), (6, 3), (7, 4)] {
            assert_eq!(
                dispatch_with(
                    &handle,
                    SubscriberId(1),
                    &commands,
                    invoke(id, "increment", Args::new()),
                ),
                Response::Returned {
                    id: RequestId(id),
                    value: Value::Int(expected)
                },
                "call {id} must be served, not refused as reentrant"
            );
        }
        assert_eq!(
            handle.get(&p("count")).expect("read should succeed"),
            Some(Value::Int(4)),
            "every write the command made must have landed"
        );
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn a_command_reached_over_text_answers_the_bytes_a_guest_expects() {
        // The registry driven the way a transport drives it: guest text in,
        // wire text out. This is the seam between this crate and the wire, and
        // the exact bytes are what the Dart and TypeScript clients decode.
        let rt = rt();
        let commands = Commands::new().with("double", |args: Args, _: &StoreHandle| {
            match args.get("n") {
                Some(Value::Int(n)) => Outcome::Returned(Value::Int(n * 2)),
                _ => Outcome::Raised(Value::map([("code", Value::Str("bad_args".into()))])),
            }
        });

        assert_eq!(
            handle_text_with(
                &rt.handle(),
                SubscriberId(1),
                &commands,
                r#"{"kind":"invoke","id":"8","command":"double","args":{"t":"m","v":{"n":{"t":"i","v":"21"}}}}"#,
            ),
            r#"{"id":"8","kind":"returned","value":{"t":"i","v":"42"}}"#
        );
        assert_eq!(
            handle_text_with(
                &rt.handle(),
                SubscriberId(1),
                &commands,
                r#"{"kind":"invoke","id":"9","command":"double","args":{"t":"m","v":{}}}"#,
            ),
            r#"{"error":{"t":"m","v":{"code":{"t":"s","v":"bad_args"}}},"id":"9","kind":"raised"}"#
        );
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn a_panicking_handler_is_caught_by_the_protocol_not_by_this_crate() {
        // This crate deliberately has no guard of its own; the one at
        // `dispatch_with` covers every `CommandHost`, including hand-written
        // ones. Pinned here so a future "defensive" `catch_unwind` added to
        // `Commands::invoke` is recognised as redundant rather than as the
        // thing keeping this passing.
        //
        // Rust prints the panic to stderr as it unwinds — expected noise.
        let rt = rt();
        let commands = Commands::new().with("boom", |_, _: &StoreHandle| -> Outcome {
            panic!("intentional panic: standing in for a broken command body");
        });
        assert!(matches!(
            dispatch_with(
                &rt.handle(),
                SubscriberId(1),
                &commands,
                invoke(10, "boom", Args::new()),
            ),
            Response::Failed { .. }
        ));
        // Still serving.
        assert!(matches!(
            dispatch_with(
                &rt.handle(),
                SubscriberId(1),
                &commands,
                invoke(11, "boom", Args::new()),
            ),
            Response::Failed { .. }
        ));
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn registering_a_name_twice_keeps_the_last_handler() {
        let rt = rt();
        let commands = Commands::new()
            .with("v", |_, _: &StoreHandle| Outcome::Returned(Value::Int(1)))
            .with("v", |_, _: &StoreHandle| Outcome::Returned(Value::Int(2)));
        assert_eq!(commands.len(), 1, "a repeat must replace, not accumulate");
        assert_eq!(
            dispatch_with(
                &rt.handle(),
                SubscriberId(1),
                &commands,
                invoke(12, "v", Args::new()),
            ),
            Response::Returned {
                id: RequestId(12),
                value: Value::Int(2)
            }
        );
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn two_surfaces_can_be_given_two_different_registries_over_one_store() {
        // The authorization claim, at the scale that can express it. One store,
        // two guests, two registries: the name one surface can reach is not a
        // name the other has at all. Nothing about the *request* differs — same
        // command name, same arguments — so the only thing deciding the outcome
        // is which registry the surface was built with, which is the property.
        let rt = rt();
        let handle = rt.handle();
        let privileged = Commands::new().with("secret.rotate_keys", |_, _: &StoreHandle| {
            Outcome::Returned(Value::Bool(true))
        });
        let sandboxed = Commands::new().with("documents.rename", |_, _: &StoreHandle| {
            Outcome::Returned(Value::Null)
        });

        assert!(matches!(
            dispatch_with(
                &handle,
                SubscriberId(1),
                &privileged,
                invoke(13, "secret.rotate_keys", Args::new()),
            ),
            Response::Returned { .. }
        ));
        assert!(
            matches!(
                dispatch_with(
                    &handle,
                    SubscriberId(2),
                    &sandboxed,
                    invoke(14, "secret.rotate_keys", Args::new()),
                ),
                Response::Failed { .. }
            ),
            "the sandboxed surface must not reach the privileged surface's command"
        );
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn debug_shows_the_names_and_not_the_handlers() {
        let commands = Commands::new()
            .with("b", |_, _: &StoreHandle| Outcome::Returned(Value::Null))
            .with("a", |_, _: &StoreHandle| Outcome::Returned(Value::Null));
        let rendered = format!("{commands:?}");
        assert!(
            rendered.contains('a') && rendered.contains('b'),
            "{rendered}"
        );
        assert_eq!(commands.names(), ["a", "b"], "names must be sorted");
    }

    #[test]
    fn a_registry_can_be_shared_across_threads() {
        // `Commands` must be `Send + Sync`, or an embedder holding one for two
        // surfaces would have to build two. Asserted as a bound rather than by
        // spawning, so the failure is a compile error naming the type.
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Commands>();
    }
}
