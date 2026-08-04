# Phase 2a — `duet-runtime` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move `duet-core`'s single-threaded `Store` onto a dedicated core thread behind a cheap, cloneable, thread-safe `StoreHandle`, and deliver notifications to the UI thread through an abstract `Sink`.

**Architecture:** A `Runtime` owns a `Store` on its own OS thread and serves requests arriving over an `mpsc` channel, replying on per-request reply channels. All store mutation therefore happens on exactly one thread with a single total order, and no lock is ever held across a call. Notifications produced by a write are handed to a `Sink` — a trait, not `tao` — so the whole crate is testable without a window system.

**Tech Stack:** Rust 1.92, edition 2024, **zero dependencies** (`std::sync::mpsc` + `std::thread`), `duet-core` as the only path dependency.

**Reference:** `docs/superpowers/specs/2026-08-04-duet-design.md` §6.2 (threading).
**Evidence:** `docs/superpowers/spikes/2026-08-04-phase0-findings.md` §B1 — Spike B measured 709/709 proxy events delivered with zero loss over 180 s, validating this model on macOS.

---

## Background for the implementer

**What already exists.** `duet-core` is complete, 141 tests, zero dependencies. Its `Store` is deliberately plain single-threaded data:

```rust
Store::new(Value) -> Store
Store::get(&self, &Path) -> Option<&Value>
Store::set(&mut self, &Path, Value) -> Result<Vec<Notification>, SetError>
Store::subscribe(&mut self, SubscriberId, Path) -> (SubscriptionId, Option<Value>)
Store::unsubscribe(&mut self, SubscriptionId) -> bool
Store::drop_subscriber(&mut self, SubscriberId) -> usize
```

`Store`, `Notification`, `Value` and `Path` are all `Send + Sync` — `duet-core`'s `lib.rs` has a static assertion locking that in. Do not break it.

**Why a thread and not a `Mutex`.** Three reasons, in order of importance:

1. **`Store::set` returns its effects as data.** A write yields `Vec<Notification>` that must be delivered somewhere. With a mutex, whoever holds the lock also has to deliver — either while holding it (blocking every other writer on I/O) or after releasing it (allowing two writes' notifications to be delivered out of order relative to the writes themselves). A single owning thread makes write order and notification order the same order, for free.
2. **The main thread must never block on store work.** Spec §6.2: the UI thread runs the tao event loop, Flutter's platform thread, *and* the webview. A mutex would let a slow store operation stall all three.
3. **No lock is held across a reply.** Callers wait on their own reply channel, not on a shared lock.

**The three execution contexts** (spec §6.2), for orientation:

```
MAIN/UI THREAD   tao event loop · Flutter platform thread · webview
       │ requests                              ▲ notifications (via Sink)
       ▼                                       │
CORE THREAD      owns Store · serialized · short work only
```

The task pool (tokio) is the third context in the spec. **It is deliberately not in this phase** — it exists to run user `#[command]` bodies, and those arrive with Phase 4's codegen. Building it now would be building for a consumer that does not exist.

**Why `Sink` is a trait.** Spec §6.2 says notifications marshal to the main thread via `tao`'s `EventLoopProxy`. If this crate depended on `tao`, none of it could be tested without a window system. Instead the crate defines `Sink`; `EventLoopProxy` implements it in Phase 2b. Tests use a channel-backed fake. This is the same decision that let `duet-core` reach 97% coverage on any machine.

**Blocking API, deliberately.** `StoreHandle`'s methods block the calling thread until the core thread replies. This is correct: the operations are microseconds of in-process work, and an async API would force an executor choice this phase has no basis to make. Phase 2b may add async wrappers.

---

## File Structure

| File | Responsibility |
|---|---|
| `crates/duet-runtime/Cargo.toml` | Manifest; only `duet-core` |
| `crates/duet-runtime/src/lib.rs` | Crate docs, module decls, re-exports, `Send`/`Sync` assertions |
| `crates/duet-runtime/src/error.rs` | `RuntimeError` |
| `crates/duet-runtime/src/sink.rs` | `Sink` trait, `SinkError`, `NullSink`, `RecordingSink` |
| `crates/duet-runtime/src/command.rs` | `CoreCommand` — the private wire between handle and core thread |
| `crates/duet-runtime/src/handle.rs` | `StoreHandle` — cloneable, thread-safe, blocking |
| `crates/duet-runtime/src/runtime.rs` | `Runtime` — spawns the core thread, owns shutdown |
| `crates/duet-runtime/tests/threading.rs` | Integration: concurrency, ordering, shutdown |

Unit tests live in `#[cfg(test)] mod tests` at the bottom of each file, per the crate convention established in `duet-core`.

---

## Standing quality bar

Every item below was a real review finding during Phase 1 that cost a round trip. Meet them up front.

**Documentation**
- Every public item gets a `///` doc comment, **including every enum variant and struct field**.
- Every `Result`-returning function gets an `# Errors` section.
- Verify doc claims against the code. A Phase 1 review found docs promising something a type did not deliver.
- If a comment states an invariant, the code must enforce it.

**Tests**
- No tautological assertions. `assert!(x.is_ok())` is almost never enough — assert the shape.
- **Pin exact counts, not loose bounds.** `assert!(n > 50)` guarding a true value of 188 would survive a regression rejecting two-thirds of valid inputs.
- **Property tests pin structure; example tests pin semantics. Include both.** In Phase 1, four algebraic property tests all passed against a mutant that only a concrete example caught.
- **Check a fixture can express the distinctions it polices.** A single-element list once left four mutants alive because "slot i" and "slot 0" were indistinguishable.
- Verify each test genuinely fails before the implementation exists.

**Concurrency-specific**
- **Never assert on timing.** No `sleep(50ms); assert!(done)`. Synchronise explicitly with channels or barriers. A timing-based test is a flaky test.
- **A test that can hang must not.** Any test waiting on a thread uses a bounded wait and fails with a clear message on timeout, rather than hanging CI forever.
- Run the suite under `--test-threads=1` **and** the default to shake out cross-test interference.

**Code**
- Functions under 50 lines; zero dependencies beyond `duet-core`.
- No `unwrap`/`expect` in non-test code.
- No `Mutex` around the `Store` — the whole point is that it is owned by one thread.

---

## Task 1: Crate scaffold and `RuntimeError`

**Files:**
- Create: `crates/duet-runtime/Cargo.toml`
- Create: `crates/duet-runtime/src/lib.rs`
- Create: `crates/duet-runtime/src/error.rs`
- Modify: `Cargo.toml` (workspace members)

- [ ] **Step 1: Add the crate to the workspace**

In the root `Cargo.toml`, change the `members` line to:

```toml
members = ["crates/duet-core", "crates/duet-runtime"]
```

Leave `exclude = ["spikes"]` untouched.

- [ ] **Step 2: Create the manifest**

Create `crates/duet-runtime/Cargo.toml`:

```toml
[package]
name = "duet-runtime"
description = "Threading runtime for Duet: owns the store on a core thread"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[dependencies]
duet-core = { path = "../duet-core" }
```

`duet-core` is the only dependency. If you find yourself adding another in this phase, stop and reconsider.

- [ ] **Step 3: Write the failing test**

Create `crates/duet-runtime/src/error.rs`:

```rust
//! Errors surfaced by the runtime.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_thread_gone_displays_actionably() {
        let rendered = RuntimeError::CoreThreadGone.to_string();
        assert!(
            rendered.contains("core thread"),
            "message should name the core thread, got: {rendered}"
        );
    }

    #[test]
    fn store_errors_are_wrapped_and_readable() {
        let inner = duet_core::SetError::IndexOutOfBounds {
            path: duet_core::Path::parse("docs[9]").expect("test path should parse"),
            index: 9,
            len: 3,
        };
        let rendered = RuntimeError::Store(inner).to_string();
        assert!(rendered.contains('9'), "should surface the index, got: {rendered}");
    }

    #[test]
    fn runtime_error_is_a_std_error() {
        fn assert_error<E: std::error::Error>() {}
        assert_error::<RuntimeError>();
    }
}
```

- [ ] **Step 4: Run test to verify it fails**

Run: `cargo test -p duet-runtime`
Expected: FAIL — `cannot find type RuntimeError in this scope`.

- [ ] **Step 5: Write the implementation**

Insert above the test module in `crates/duet-runtime/src/error.rs`:

```rust
use duet_core::SetError;

/// Why a runtime operation could not be completed.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RuntimeError {
    /// The core thread is no longer running, so the request could not be
    /// served and no reply will arrive.
    ///
    /// This means the runtime was shut down, or the core thread panicked. It is
    /// terminal: every subsequent call on the same handle will also fail.
    CoreThreadGone,
    /// The store rejected the write. Carries `duet-core`'s error unchanged.
    Store(SetError),
}

impl std::fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RuntimeError::CoreThreadGone => {
                write!(f, "the runtime core thread is no longer running")
            }
            RuntimeError::Store(e) => write!(f, "store rejected the write: {e}"),
        }
    }
}

impl std::error::Error for RuntimeError {}

impl From<SetError> for RuntimeError {
    fn from(e: SetError) -> Self {
        RuntimeError::Store(e)
    }
}
```

- [ ] **Step 6: Create the crate root**

Create `crates/duet-runtime/src/lib.rs`:

```rust
//! Threading runtime for Duet.
//!
//! [`duet_core::Store`] is deliberately single-threaded plain data. This crate
//! moves it onto a dedicated **core thread** and hands out cheap, cloneable
//! [`StoreHandle`]s that any thread may use.
//!
//! # Why a thread rather than a mutex
//!
//! A write returns its effects as data — `Vec<Notification>` — which must be
//! delivered somewhere. Under a mutex, the writer would have to deliver either
//! while holding the lock (stalling every other writer) or after releasing it
//! (allowing two writes' notifications to be reordered relative to the writes).
//! One owning thread makes write order and notification order the same order.
//!
//! The main thread also runs the tao event loop, Flutter's platform thread and
//! the webview, so it must never block on store work.
//!
//! # Delivery
//!
//! Notifications are handed to a [`Sink`]. That is a trait rather than `tao`'s
//! `EventLoopProxy` so this crate stays testable without a window system;
//! Phase 2b supplies the real implementation.

#![deny(missing_docs)]

pub mod error;
pub mod sink;

pub use error::RuntimeError;

/// These bounds are load-bearing: the core thread owns the `Store` and receives
/// values from other threads. Asserted here so a change that breaks them fails
/// at its own source rather than at an integration point.
const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<RuntimeError>();
};
```

- [ ] **Step 7: Run tests to verify they pass**

Run: `cargo test -p duet-runtime`
Expected: PASS — 3 passed.

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml crates/duet-runtime/
git commit -m "feat(runtime): scaffold duet-runtime with RuntimeError"
```

---

## Task 2: The `Sink` trait

**Files:**
- Create: `crates/duet-runtime/src/sink.rs`
- Modify: `crates/duet-runtime/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/duet-runtime/src/sink.rs`:

```rust
//! Where notifications go once the core thread has produced them.

#[cfg(test)]
mod tests {
    use super::*;
    use duet_core::{Notification, Patch, Path, SubscriberId, SubscriptionId, Value};

    fn note(n: u64) -> Notification {
        Notification {
            subscriber: SubscriberId(n),
            subscription: SubscriptionId(n),
            patch: Patch {
                path: Path::parse("editor.zoom").expect("test path should parse"),
                value: Value::Int(n as i64),
            },
        }
    }

    #[test]
    fn recording_sink_captures_batches_in_order() {
        let sink = RecordingSink::new();
        sink.deliver(vec![note(1), note(2)]).expect("delivery should succeed");
        sink.deliver(vec![note(3)]).expect("delivery should succeed");

        let batches = sink.batches();
        assert_eq!(batches.len(), 2, "two deliver calls should record two batches");
        assert_eq!(batches[0].len(), 2);
        assert_eq!(batches[1].len(), 1);
        assert_eq!(batches[0][0].subscriber, SubscriberId(1));
        assert_eq!(batches[1][0].subscriber, SubscriberId(3));
    }

    #[test]
    fn recording_sink_flattens_for_convenience() {
        let sink = RecordingSink::new();
        sink.deliver(vec![note(1), note(2)]).expect("delivery should succeed");
        sink.deliver(vec![note(3)]).expect("delivery should succeed");

        let all = sink.notifications();
        assert_eq!(all.len(), 3);
        assert_eq!(all[2].subscriber, SubscriberId(3));
    }

    #[test]
    fn null_sink_accepts_everything_and_records_nothing() {
        let sink = NullSink;
        assert_eq!(sink.deliver(vec![note(1)]), Ok(()));
    }

    #[test]
    fn empty_batches_are_still_recorded() {
        // A write that matches no subscription produces an empty batch. The
        // core thread may skip delivering it, but the Sink contract must not
        // reject one.
        let sink = RecordingSink::new();
        sink.deliver(Vec::new()).expect("empty delivery should succeed");
        assert_eq!(sink.batches().len(), 1);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p duet-runtime`
Expected: FAIL — `cannot find type RecordingSink in this scope`.

- [ ] **Step 3: Write the implementation**

Insert above the test module in `crates/duet-runtime/src/sink.rs`:

```rust
use std::sync::{Arc, Mutex};

use duet_core::Notification;

/// Why a sink could not accept a batch.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SinkError {
    /// The destination is gone — for example the UI event loop has exited.
    ///
    /// The core thread treats this as non-fatal: it keeps serving requests, so
    /// a dead UI does not take the store down with it.
    Closed,
}

impl std::fmt::Display for SinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SinkError::Closed => write!(f, "notification sink is closed"),
        }
    }
}

impl std::error::Error for SinkError {}

/// Receives batches of notifications produced by a write.
///
/// Implementations marshal the batch to wherever it needs to be delivered. In
/// Phase 2b the real implementation wraps `tao`'s `EventLoopProxy` to hop onto
/// the UI thread; keeping this a trait is what lets the runtime be tested with
/// no window system present.
///
/// One batch corresponds to exactly one successful write. Implementations must
/// not reorder or merge batches: write order is notification order, and that is
/// the guarantee the core thread exists to provide.
pub trait Sink: Send + 'static {
    /// Accepts one batch.
    ///
    /// # Errors
    ///
    /// Returns [`SinkError::Closed`] when the destination no longer exists. The
    /// core thread logs and continues; it does not shut down.
    fn deliver(&self, batch: Vec<Notification>) -> Result<(), SinkError>;
}

/// A sink that discards everything. Useful for tests that only care about
/// store state, and as a default before a UI exists.
#[derive(Debug, Clone, Copy, Default)]
pub struct NullSink;

impl Sink for NullSink {
    fn deliver(&self, _batch: Vec<Notification>) -> Result<(), SinkError> {
        Ok(())
    }
}

/// A sink that records every batch it receives, for assertions in tests.
///
/// Cloning shares the same underlying recording, so a clone may be handed to
/// the runtime while the original is used to assert.
#[derive(Debug, Clone, Default)]
pub struct RecordingSink {
    batches: Arc<Mutex<Vec<Vec<Notification>>>>,
}

impl RecordingSink {
    /// Creates an empty recorder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Every batch received, in delivery order.
    ///
    /// # Panics
    ///
    /// Panics if a previous holder of the internal lock panicked. This is a
    /// test helper, so surfacing that loudly is preferable to masking it.
    pub fn batches(&self) -> Vec<Vec<Notification>> {
        self.batches.lock().expect("recording lock poisoned").clone()
    }

    /// Every notification received, flattened across batches.
    ///
    /// # Panics
    ///
    /// Panics if a previous holder of the internal lock panicked.
    pub fn notifications(&self) -> Vec<Notification> {
        self.batches().into_iter().flatten().collect()
    }
}

impl Sink for RecordingSink {
    fn deliver(&self, batch: Vec<Notification>) -> Result<(), SinkError> {
        self.batches
            .lock()
            .map_err(|_| SinkError::Closed)?
            .push(batch);
        Ok(())
    }
}
```

Note `RecordingSink` uses a `Mutex`, which is fine — it guards a test recording, not the `Store`. The rule "no mutex around the Store" is about the store specifically.

- [ ] **Step 4: Export from `lib.rs`**

Update the re-export block in `crates/duet-runtime/src/lib.rs`:

```rust
pub use error::RuntimeError;
pub use sink::{NullSink, RecordingSink, Sink, SinkError};
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p duet-runtime`
Expected: PASS — 7 passed.

- [ ] **Step 6: Commit**

```bash
git add crates/duet-runtime/src/
git commit -m "feat(runtime): add Sink trait with recording and null impls"
```

---

## Task 3: `CoreCommand` — the wire between handle and thread

**Files:**
- Create: `crates/duet-runtime/src/command.rs`
- Modify: `crates/duet-runtime/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/duet-runtime/src/command.rs`:

```rust
//! The private request type carried from a handle to the core thread.

#[cfg(test)]
mod tests {
    use super::*;
    use duet_core::{Path, Value};
    use std::sync::mpsc;

    #[test]
    fn commands_carry_their_reply_channel() {
        let (tx, rx) = mpsc::channel();
        let cmd = CoreCommand::Get {
            path: Path::parse("editor.zoom").expect("test path should parse"),
            reply: tx,
        };

        // Simulate the core thread answering.
        match cmd {
            CoreCommand::Get { reply, .. } => {
                reply.send(Some(Value::Float(1.5))).expect("reply should send");
            }
            other => panic!("expected Get, got {other:?}"),
        }

        assert_eq!(rx.recv().expect("reply should arrive"), Some(Value::Float(1.5)));
    }

    #[test]
    fn command_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<CoreCommand>();
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p duet-runtime`
Expected: FAIL — `cannot find type CoreCommand in this scope`.

- [ ] **Step 3: Write the implementation**

Insert above the test module in `crates/duet-runtime/src/command.rs`:

```rust
use std::sync::mpsc::Sender;

use duet_core::{Path, SetError, SubscriberId, SubscriptionId, Value};

/// A request from a [`crate::StoreHandle`] to the core thread.
///
/// Every variant carries its own reply channel, so a caller waits only on its
/// own request and never on a shared lock. Dropping the reply sender without
/// sending — which happens if the core thread dies mid-request — closes the
/// caller's receiver, and the caller reports
/// [`crate::RuntimeError::CoreThreadGone`] rather than hanging.
///
/// This type is crate-private: it is an implementation detail of how the handle
/// talks to the thread, not part of the public API.
#[derive(Debug)]
pub(crate) enum CoreCommand {
    /// Read the value at a path.
    Get {
        /// Path to read.
        path: Path,
        /// Where to send the value, or `None` if the path is absent.
        reply: Sender<Option<Value>>,
    },
    /// Write a value, producing notifications for overlapping subscriptions.
    Set {
        /// Path to write.
        path: Path,
        /// Value to write.
        value: Value,
        /// Where to send success, or the store's rejection.
        reply: Sender<Result<(), SetError>>,
    },
    /// Register a subscription and take a snapshot of its path.
    Subscribe {
        /// Who is subscribing.
        subscriber: SubscriberId,
        /// Path to watch.
        path: Path,
        /// Where to send the new id and the current value at that path.
        reply: Sender<(SubscriptionId, Option<Value>)>,
    },
    /// Remove one subscription.
    Unsubscribe {
        /// Subscription to remove.
        id: SubscriptionId,
        /// Where to send whether it was present.
        reply: Sender<bool>,
    },
    /// Remove every subscription held by a subscriber. Used when a surface
    /// goes `Cold`.
    DropSubscriber {
        /// Whose subscriptions to remove.
        subscriber: SubscriberId,
        /// Where to send how many were removed.
        reply: Sender<usize>,
    },
    /// Stop the core thread after draining requests already queued ahead of
    /// this one.
    Shutdown {
        /// Signalled once the thread is about to exit.
        reply: Sender<()>,
    },
}
```

- [ ] **Step 4: Declare the module**

Add to `crates/duet-runtime/src/lib.rs`, above the `pub mod` lines:

```rust
mod command;
```

It is private — do not `pub use` anything from it.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p duet-runtime`
Expected: PASS — 9 passed.

- [ ] **Step 6: Commit**

```bash
git add crates/duet-runtime/src/
git commit -m "feat(runtime): add CoreCommand request type"
```

---

## Task 4: `Runtime` and `StoreHandle` — spawn, get, set, shutdown

This is the centre of the crate.

**Files:**
- Create: `crates/duet-runtime/src/handle.rs`
- Create: `crates/duet-runtime/src/runtime.rs`
- Modify: `crates/duet-runtime/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/duet-runtime/src/runtime.rs`:

```rust
//! The core thread: owns the store, serves requests, delivers notifications.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sink::{NullSink, RecordingSink};
    use duet_core::{Path, Value};

    fn sample() -> Value {
        Value::map([(
            "editor",
            Value::map([("zoom", Value::Float(1.0)), ("theme", Value::Str("dark".into()))]),
        )])
    }

    fn p(s: &str) -> Path {
        Path::parse(s).expect("test path should parse")
    }

    #[test]
    fn get_reads_through_to_the_store() {
        let rt = Runtime::spawn(sample(), NullSink);
        let handle = rt.handle();
        assert_eq!(handle.get(&p("editor.zoom")), Ok(Some(Value::Float(1.0))));
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn get_returns_owned_values_not_references() {
        // Deliberate API difference from duet_core::Store::get, which returns
        // Option<&Value>: a reference cannot cross a thread boundary.
        let rt = Runtime::spawn(sample(), NullSink);
        let handle = rt.handle();
        let v: Option<Value> = handle.get(&p("editor.theme")).expect("get should succeed");
        assert_eq!(v, Some(Value::Str("dark".into())));
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn get_on_missing_path_is_none_not_an_error() {
        let rt = Runtime::spawn(sample(), NullSink);
        assert_eq!(rt.handle().get(&p("editor.nope")), Ok(None));
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn set_writes_and_is_visible_to_a_later_get() {
        let rt = Runtime::spawn(sample(), NullSink);
        let handle = rt.handle();
        handle.set(&p("editor.zoom"), Value::Float(2.5)).expect("set should succeed");
        assert_eq!(handle.get(&p("editor.zoom")), Ok(Some(Value::Float(2.5))));
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn rejected_write_surfaces_the_store_error() {
        let rt = Runtime::spawn(sample(), NullSink);
        let err = rt
            .handle()
            .set(&p("nope.deeper"), Value::Null)
            .expect_err("writing through a missing key must fail");
        assert!(
            matches!(err, crate::RuntimeError::Store(_)),
            "expected a wrapped store error, got {err:?}"
        );
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn rejected_write_leaves_state_untouched() {
        let rt = Runtime::spawn(sample(), NullSink);
        let handle = rt.handle();
        let _ = handle.set(&p("nope.deeper"), Value::Null);
        assert_eq!(handle.get(&p("editor.zoom")), Ok(Some(Value::Float(1.0))));
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn handle_is_cloneable_and_clones_share_one_store() {
        let rt = Runtime::spawn(sample(), NullSink);
        let a = rt.handle();
        let b = a.clone();
        a.set(&p("editor.zoom"), Value::Float(9.0)).expect("set should succeed");
        assert_eq!(b.get(&p("editor.zoom")), Ok(Some(Value::Float(9.0))));
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn calls_after_shutdown_report_core_thread_gone() {
        let rt = Runtime::spawn(sample(), NullSink);
        let handle = rt.handle();
        rt.shutdown().expect("shutdown should succeed");
        assert_eq!(
            handle.get(&p("editor.zoom")),
            Err(crate::RuntimeError::CoreThreadGone)
        );
    }

    #[test]
    fn write_delivers_a_batch_to_the_sink() {
        let sink = RecordingSink::new();
        let rt = Runtime::spawn(sample(), sink.clone());
        let handle = rt.handle();
        handle
            .subscribe(duet_core::SubscriberId(1), p("editor.zoom"))
            .expect("subscribe should succeed");
        handle.set(&p("editor.zoom"), Value::Float(2.0)).expect("set should succeed");
        rt.shutdown().expect("shutdown should succeed");

        let notes = sink.notifications();
        assert_eq!(notes.len(), 1, "one overlapping subscription means one notification");
        assert_eq!(notes[0].patch.path, p("editor.zoom"));
        assert_eq!(notes[0].patch.value, Value::Float(2.0));
    }

    #[test]
    fn rejected_write_delivers_nothing() {
        let sink = RecordingSink::new();
        let rt = Runtime::spawn(sample(), sink.clone());
        let handle = rt.handle();
        handle
            .subscribe(duet_core::SubscriberId(1), duet_core::Path::root())
            .expect("subscribe should succeed");
        let _ = handle.set(&p("nope.deeper"), Value::Null);
        rt.shutdown().expect("shutdown should succeed");

        assert!(
            sink.notifications().is_empty(),
            "a rejected write must produce no notifications"
        );
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p duet-runtime`
Expected: FAIL — `cannot find type Runtime in this scope`.

- [ ] **Step 3: Write `StoreHandle`**

Create `crates/duet-runtime/src/handle.rs`:

```rust
//! A cheap, cloneable, thread-safe handle to the store.

use std::sync::mpsc::{self, Sender};

use duet_core::{Path, SubscriberId, SubscriptionId, Value};

use crate::command::CoreCommand;
use crate::error::RuntimeError;

/// A thread-safe handle to the store owned by the core thread.
///
/// Cloning is cheap — it clones a channel sender — and every clone talks to the
/// same store. Handles may be sent to and used from any thread.
///
/// All methods **block** the calling thread until the core thread replies. The
/// operations are microseconds of in-process work, and a blocking API avoids
/// forcing an async executor choice on callers. Never call these from the core
/// thread itself: it would wait for a reply it is responsible for producing.
#[derive(Debug, Clone)]
pub struct StoreHandle {
    tx: Sender<CoreCommand>,
}

impl StoreHandle {
    pub(crate) fn new(tx: Sender<CoreCommand>) -> Self {
        StoreHandle { tx }
    }

    /// Sends a request and waits for its reply.
    ///
    /// Both a failed send and a closed reply channel mean the core thread is
    /// gone, so both map to the same error. This is the single place that
    /// blocking round-trip is implemented.
    fn call<T>(&self, make: impl FnOnce(Sender<T>) -> CoreCommand) -> Result<T, RuntimeError> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.tx
            .send(make(reply_tx))
            .map_err(|_| RuntimeError::CoreThreadGone)?;
        reply_rx.recv().map_err(|_| RuntimeError::CoreThreadGone)
    }

    /// Reads the value at `path`, or `None` if it is absent.
    ///
    /// Returns an owned `Value` rather than a reference, because a reference
    /// into the core thread's store cannot cross a thread boundary.
    ///
    /// # Errors
    ///
    /// [`RuntimeError::CoreThreadGone`] if the runtime has shut down.
    pub fn get(&self, path: &Path) -> Result<Option<Value>, RuntimeError> {
        let path = path.clone();
        self.call(|reply| CoreCommand::Get { path, reply })
    }

    /// Writes `value` at `path`, notifying every overlapping subscription.
    ///
    /// # Errors
    ///
    /// [`RuntimeError::Store`] if the store rejected the write — in which case
    /// nothing was mutated and no notifications were produced.
    /// [`RuntimeError::CoreThreadGone`] if the runtime has shut down.
    pub fn set(&self, path: &Path, value: Value) -> Result<(), RuntimeError> {
        let path = path.clone();
        self.call(|reply| CoreCommand::Set { path, value, reply })?
            .map_err(RuntimeError::Store)
    }

    /// Registers a subscription, returning its id and a snapshot of `path`.
    ///
    /// # Errors
    ///
    /// [`RuntimeError::CoreThreadGone`] if the runtime has shut down.
    pub fn subscribe(
        &self,
        subscriber: SubscriberId,
        path: Path,
    ) -> Result<(SubscriptionId, Option<Value>), RuntimeError> {
        self.call(|reply| CoreCommand::Subscribe {
            subscriber,
            path,
            reply,
        })
    }

    /// Removes one subscription, returning whether it was present.
    ///
    /// # Errors
    ///
    /// [`RuntimeError::CoreThreadGone`] if the runtime has shut down.
    pub fn unsubscribe(&self, id: SubscriptionId) -> Result<bool, RuntimeError> {
        self.call(|reply| CoreCommand::Unsubscribe { id, reply })
    }

    /// Removes every subscription held by `subscriber`, returning how many.
    ///
    /// Called when a surface goes `Cold`.
    ///
    /// # Errors
    ///
    /// [`RuntimeError::CoreThreadGone`] if the runtime has shut down.
    pub fn drop_subscriber(&self, subscriber: SubscriberId) -> Result<usize, RuntimeError> {
        self.call(|reply| CoreCommand::DropSubscriber { subscriber, reply })
    }
}
```

- [ ] **Step 4: Write `Runtime`**

Insert above the test module in `crates/duet-runtime/src/runtime.rs`:

```rust
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

use duet_core::{Store, Value};

use crate::command::CoreCommand;
use crate::error::RuntimeError;
use crate::handle::StoreHandle;
use crate::sink::Sink;

/// Owns the store on a dedicated thread and serves requests from handles.
///
/// Dropping a `Runtime` without calling [`Runtime::shutdown`] closes the request
/// channel, which stops the core thread, but does not wait for it to finish. Use
/// `shutdown` when you need to know the thread has exited.
#[derive(Debug)]
pub struct Runtime {
    tx: Sender<CoreCommand>,
    join: JoinHandle<()>,
}

impl Runtime {
    /// Starts a core thread owning `root`, delivering notifications to `sink`.
    pub fn spawn<S: Sink>(root: Value, sink: S) -> Runtime {
        let (tx, rx) = mpsc::channel();
        let join = thread::Builder::new()
            .name("duet-core".to_string())
            .spawn(move || core_loop(Store::new(root), rx, sink))
            .expect("spawning the core thread should not fail");
        Runtime { tx, join }
    }

    /// Returns a handle. Call as many times as needed; handles are cheap and
    /// all clones address the same store.
    pub fn handle(&self) -> StoreHandle {
        StoreHandle::new(self.tx.clone())
    }

    /// Stops the core thread and waits for it to exit.
    ///
    /// Requests already queued ahead of the shutdown request are served first,
    /// so an in-flight write is never lost. Handles outliving this call report
    /// [`RuntimeError::CoreThreadGone`].
    ///
    /// Takes `self` by value, so calling it twice is a compile error rather
    /// than a runtime condition. That is why no test asserts idempotence —
    /// the type system enforces it.
    ///
    /// # Errors
    ///
    /// [`RuntimeError::CoreThreadGone`] if the thread had already stopped —
    /// for example because it panicked.
    pub fn shutdown(self) -> Result<(), RuntimeError> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.tx
            .send(CoreCommand::Shutdown { reply: reply_tx })
            .map_err(|_| RuntimeError::CoreThreadGone)?;
        let _ = reply_rx.recv();
        drop(self.tx);
        self.join.join().map_err(|_| RuntimeError::CoreThreadGone)
    }
}

/// The core thread's whole life: take one request, serve it, repeat.
///
/// Exits when a `Shutdown` arrives, or when every handle has been dropped and
/// the channel closes.
fn core_loop<S: Sink>(mut store: Store, rx: Receiver<CoreCommand>, sink: S) {
    while let Ok(cmd) = rx.recv() {
        match cmd {
            CoreCommand::Get { path, reply } => {
                let _ = reply.send(store.get(&path).cloned());
            }
            CoreCommand::Set { path, value, reply } => {
                match store.set(&path, value) {
                    Ok(notifications) => {
                        // Reply before delivering, so a slow sink cannot make
                        // the writer wait. Delivery order still matches write
                        // order because this thread is the only deliverer.
                        let _ = reply.send(Ok(()));
                        if !notifications.is_empty() {
                            // A closed sink is not fatal: a dead UI must not
                            // take the store down with it.
                            let _ = sink.deliver(notifications);
                        }
                    }
                    Err(e) => {
                        let _ = reply.send(Err(e));
                    }
                }
            }
            CoreCommand::Subscribe {
                subscriber,
                path,
                reply,
            } => {
                let _ = reply.send(store.subscribe(subscriber, path));
            }
            CoreCommand::Unsubscribe { id, reply } => {
                let _ = reply.send(store.unsubscribe(id));
            }
            CoreCommand::DropSubscriber { subscriber, reply } => {
                let _ = reply.send(store.drop_subscriber(subscriber));
            }
            CoreCommand::Shutdown { reply } => {
                let _ = reply.send(());
                return;
            }
        }
    }
}
```

Every `reply.send` result is deliberately discarded: a caller may have given up and dropped its receiver, and that is not the core thread's problem.

- [ ] **Step 5: Wire up `lib.rs`**

Update `crates/duet-runtime/src/lib.rs`'s module and re-export blocks:

```rust
mod command;
pub mod error;
pub mod handle;
pub mod runtime;
pub mod sink;

pub use error::RuntimeError;
pub use handle::StoreHandle;
pub use runtime::Runtime;
pub use sink::{NullSink, RecordingSink, Sink, SinkError};
```

And extend the static assertion:

```rust
const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<RuntimeError>();
    assert_send_sync::<StoreHandle>();
};
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test -p duet-runtime`
Expected: PASS — 19 passed.

- [ ] **Step 7: Commit**

```bash
git add crates/duet-runtime/src/
git commit -m "feat(runtime): add Runtime core thread and StoreHandle"
```

---

## Task 5: Concurrency properties

The reason this crate exists. Read the concurrency bar in "Standing quality bar" before starting: **no timing assertions, no test that can hang.**

**Files:**
- Create: `crates/duet-runtime/tests/threading.rs`

- [ ] **Step 1: Write the failing tests**

Create `crates/duet-runtime/tests/threading.rs`:

```rust
//! Integration tests for the threading guarantees the runtime exists to provide.

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use duet_core::{Path, SubscriberId, Value};
use duet_runtime::{NullSink, RecordingSink, Runtime, StoreHandle};

/// Longest any test will wait for a worker before declaring failure. Generous
/// enough never to fire on a loaded CI box, bounded so a deadlock fails the
/// suite instead of hanging it forever.
const JOIN_TIMEOUT: Duration = Duration::from_secs(30);

fn p(s: &str) -> Path {
    Path::parse(s).expect("test path should parse")
}

fn counter_tree() -> Value {
    Value::map([("counter", Value::Int(0)), ("log", Value::Int(0))])
}

/// Joins a set of workers, failing rather than hanging if one is stuck.
///
/// `std::thread::JoinHandle` has no timed join, so each worker signals a
/// channel on completion and we wait on that with a timeout.
fn join_all_or_fail(done: mpsc::Receiver<usize>, expected: usize) {
    for i in 0..expected {
        done.recv_timeout(JOIN_TIMEOUT)
            .unwrap_or_else(|_| panic!("worker {i} did not finish within {JOIN_TIMEOUT:?} — likely deadlock"));
    }
}

#[test]
fn handles_work_from_many_threads_with_no_lost_updates() {
    const WORKERS: usize = 8;
    const WRITES_PER_WORKER: usize = 50;

    let rt = Runtime::spawn(counter_tree(), NullSink);
    let (done_tx, done_rx) = mpsc::channel();

    let mut workers = Vec::new();
    for w in 0..WORKERS {
        let handle: StoreHandle = rt.handle();
        let done = done_tx.clone();
        workers.push(thread::spawn(move || {
            for i in 0..WRITES_PER_WORKER {
                let value = Value::Int((w * WRITES_PER_WORKER + i) as i64);
                handle
                    .set(&p("counter"), value)
                    .expect("write from worker should succeed");
            }
            done.send(w).expect("done signal should send");
        }));
    }
    drop(done_tx);

    join_all_or_fail(done_rx, WORKERS);
    for w in workers {
        w.join().expect("worker thread should not panic");
    }

    // Every write was accepted; the final value is whichever landed last.
    // The point is that all 400 completed and none deadlocked or errored.
    let final_value = rt.handle().get(&p("counter")).expect("read should succeed");
    assert!(final_value.is_some(), "counter should still exist after concurrent writes");

    rt.shutdown().expect("shutdown should succeed");
}

#[test]
fn every_write_produces_exactly_one_notification_batch() {
    const WORKERS: usize = 4;
    const WRITES_PER_WORKER: usize = 25;
    const TOTAL: usize = WORKERS * WRITES_PER_WORKER;

    let sink = RecordingSink::new();
    let rt = Runtime::spawn(counter_tree(), sink.clone());

    rt.handle()
        .subscribe(SubscriberId(1), p("counter"))
        .expect("subscribe should succeed");

    let (done_tx, done_rx) = mpsc::channel();
    let mut workers = Vec::new();
    for w in 0..WORKERS {
        let handle = rt.handle();
        let done = done_tx.clone();
        workers.push(thread::spawn(move || {
            for i in 0..WRITES_PER_WORKER {
                handle
                    .set(&p("counter"), Value::Int(i as i64))
                    .expect("write should succeed");
            }
            done.send(w).expect("done signal should send");
        }));
    }
    drop(done_tx);

    join_all_or_fail(done_rx, WORKERS);
    for w in workers {
        w.join().expect("worker thread should not panic");
    }
    rt.shutdown().expect("shutdown should succeed");

    // Exactly one batch per successful write, each with exactly one
    // notification (one matching subscription). No lost, no duplicated.
    let batches = sink.batches();
    assert_eq!(batches.len(), TOTAL, "one batch per write, none lost or duplicated");
    for (n, batch) in batches.iter().enumerate() {
        assert_eq!(batch.len(), 1, "batch {n} should hold exactly one notification");
        assert_eq!(batch[0].subscriber, SubscriberId(1));
    }
}

#[test]
fn writes_from_one_thread_are_delivered_in_that_order() {
    // Ordering is only meaningful per-writer: with several writers the
    // interleaving is legitimately non-deterministic. One writer, so the
    // expected sequence is exact.
    const WRITES: i64 = 100;

    let sink = RecordingSink::new();
    let rt = Runtime::spawn(counter_tree(), sink.clone());
    let handle = rt.handle();

    handle
        .subscribe(SubscriberId(1), p("counter"))
        .expect("subscribe should succeed");

    for i in 0..WRITES {
        handle.set(&p("counter"), Value::Int(i)).expect("write should succeed");
    }
    rt.shutdown().expect("shutdown should succeed");

    let observed: Vec<i64> = sink
        .notifications()
        .into_iter()
        .filter_map(|n| match n.patch.value {
            Value::Int(i) => Some(i),
            _ => None,
        })
        .collect();

    let expected: Vec<i64> = (0..WRITES).collect();
    assert_eq!(observed, expected, "notification order must match write order");
}

#[test]
fn shutdown_serves_requests_already_queued() {
    // A write issued before shutdown must land, not be discarded.
    let rt = Runtime::spawn(counter_tree(), NullSink);
    let handle = rt.handle();

    handle.set(&p("counter"), Value::Int(42)).expect("write should succeed");
    let observed = handle.get(&p("counter")).expect("read should succeed");
    assert_eq!(observed, Some(Value::Int(42)));

    rt.shutdown().expect("shutdown should succeed");
    assert_eq!(
        handle.get(&p("counter")),
        Err(duet_runtime::RuntimeError::CoreThreadGone)
    );
}

#[test]
fn a_handle_moved_to_another_thread_still_works() {
    let rt = Runtime::spawn(counter_tree(), NullSink);
    let handle = rt.handle();

    let worker = thread::spawn(move || {
        handle.set(&p("counter"), Value::Int(7)).expect("write should succeed");
        handle.get(&p("counter")).expect("read should succeed")
    });

    let observed = worker.join().expect("worker should not panic");
    assert_eq!(observed, Some(Value::Int(7)));

    rt.shutdown().expect("shutdown should succeed");
}

#[test]
fn dropping_the_runtime_without_shutdown_stops_the_thread() {
    let rt = Runtime::spawn(counter_tree(), NullSink);
    let handle = rt.handle();
    drop(rt);

    // The runtime's sender is gone, but this handle holds a clone, so the
    // channel is still open and the thread is still serving.
    assert_eq!(handle.get(&p("counter")), Ok(Some(Value::Int(0))));

    // Once the last handle goes, the channel closes and the thread exits.
    drop(handle);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p duet-runtime --test threading`
Expected: FAIL — compilation error if any name is missing from `duet-runtime`'s re-exports. If it compiles and passes immediately, the earlier tasks were done correctly; still run Step 4.

- [ ] **Step 3: Fix any missing re-exports**

Ensure `crates/duet-runtime/src/lib.rs` re-exports exactly:

```rust
pub use error::RuntimeError;
pub use handle::StoreHandle;
pub use runtime::Runtime;
pub use sink::{NullSink, RecordingSink, Sink, SinkError};
```

- [ ] **Step 4: Run the whole suite, both threading modes**

Run: `cargo test -p duet-runtime`
Expected: PASS — 19 unit, 6 integration.

Run: `cargo test -p duet-runtime -- --test-threads=1`
Expected: PASS — identical counts. A difference between these two runs means cross-test interference; investigate rather than ignore.

- [ ] **Step 5: Commit**

```bash
git add crates/duet-runtime/tests/
git commit -m "test(runtime): pin concurrency, ordering and shutdown guarantees"
```

---

## Task 6: Survive a panicking sink

A `Sink` implementation is supplied by the caller and may misbehave. The core thread owns the only copy of the store, so if it dies the whole application loses its state. Callers must find out, rather than hang.

**Files:**
- Modify: `crates/duet-runtime/src/runtime.rs`

- [ ] **Step 1: Write the failing test**

Add inside `mod tests` in `crates/duet-runtime/src/runtime.rs`:

```rust
    /// A sink that panics on its first delivery, standing in for a buggy
    /// caller-supplied implementation.
    #[derive(Debug, Clone, Default)]
    struct PanickingSink;

    impl crate::sink::Sink for PanickingSink {
        fn deliver(
            &self,
            _batch: Vec<duet_core::Notification>,
        ) -> Result<(), crate::sink::SinkError> {
            panic!("sink panicked on purpose");
        }
    }

    #[test]
    fn a_panicking_sink_does_not_hang_callers() {
        let rt = Runtime::spawn(sample(), PanickingSink);
        let handle = rt.handle();
        handle
            .subscribe(duet_core::SubscriberId(1), p("editor.zoom"))
            .expect("subscribe should succeed");

        // This write's notification delivery panics the core thread. The write
        // itself already replied, so this call succeeds.
        let _ = handle.set(&p("editor.zoom"), Value::Float(2.0));

        // The decisive assertion: a later call must return an error rather than
        // block forever waiting for a thread that no longer exists.
        let result = handle.get(&p("editor.zoom"));
        assert_eq!(
            result,
            Err(crate::RuntimeError::CoreThreadGone),
            "a dead core thread must surface as an error, never a hang"
        );
    }

    #[test]
    fn a_closed_sink_does_not_stop_the_core_thread() {
        /// A sink that always reports itself closed, standing in for a UI whose
        /// event loop has exited.
        #[derive(Debug, Clone, Default)]
        struct ClosedSink;

        impl crate::sink::Sink for ClosedSink {
            fn deliver(
                &self,
                _batch: Vec<duet_core::Notification>,
            ) -> Result<(), crate::sink::SinkError> {
                Err(crate::sink::SinkError::Closed)
            }
        }

        let rt = Runtime::spawn(sample(), ClosedSink);
        let handle = rt.handle();
        handle
            .subscribe(duet_core::SubscriberId(1), p("editor.zoom"))
            .expect("subscribe should succeed");

        handle.set(&p("editor.zoom"), Value::Float(2.0)).expect("write should succeed");

        // A dead UI must not take the store down with it.
        assert_eq!(
            handle.get(&p("editor.zoom")),
            Ok(Some(Value::Float(2.0))),
            "the store must keep serving after the sink reports Closed"
        );

        rt.shutdown().expect("shutdown should succeed");
    }
```

- [ ] **Step 2: Run tests to verify behaviour**

Run: `cargo test -p duet-runtime`

`a_closed_sink_does_not_stop_the_core_thread` should already PASS — `core_loop` discards the sink's result.

`a_panicking_sink_does_not_hang_callers` should also PASS, because a panicking thread drops the receiver and `StoreHandle::call` maps a closed channel to `CoreThreadGone`. **If it hangs instead, that is a real bug** — the test exists to prove the design, and Step 3 documents why it holds.

Expected: PASS — 21 passed. Rust prints the panic message to stderr; that is expected noise, not a failure.

- [ ] **Step 3: Document the guarantee in the code**

Add to `core_loop`'s doc comment in `crates/duet-runtime/src/runtime.rs`:

```rust
/// The core thread's whole life: take one request, serve it, repeat.
///
/// Exits when a `Shutdown` arrives, or when every handle has been dropped and
/// the channel closes.
///
/// # Panic safety
///
/// If this thread panics — most plausibly inside a caller-supplied
/// [`crate::Sink`] — the request `Receiver` is dropped as the stack unwinds.
/// Every pending and future `StoreHandle` call then fails its `send` or `recv`
/// and reports [`RuntimeError::CoreThreadGone`]. Callers therefore observe an
/// error rather than hanging on a thread that no longer exists. The store's
/// contents are lost with the thread; the runtime does not attempt to restart
/// it, because a store that silently resets would be worse than one that
/// reports itself gone.
```

- [ ] **Step 4: Run tests again**

Run: `cargo test -p duet-runtime`
Expected: PASS — 21 unit, 6 integration.

- [ ] **Step 5: Commit**

```bash
git add crates/duet-runtime/src/runtime.rs
git commit -m "test(runtime): prove a dead core thread surfaces as an error, not a hang"
```

---

## Task 7: Coverage gate and CI

**Files:**
- Modify: `.github/workflows/core.yml`

- [ ] **Step 1: Measure coverage**

Run: `cargo llvm-cov -p duet-runtime --summary-only`

`cargo-llvm-cov` 0.8.7 is already installed; do not install it. This forces an instrumented rebuild taking a few minutes — be patient.

Report the real numbers. If any file is below 90% line coverage, read the report and add tests for those specific branches. **Do not lower the threshold.**

- [ ] **Step 2: Confirm the gate passes**

Run: `cargo llvm-cov -p duet-runtime --fail-under-lines 90`
Expected: exit 0.

- [ ] **Step 3: Extend CI to both crates**

Replace `.github/workflows/core.yml` with:

```yaml
name: duet

on:
  push:
    paths:
      - 'crates/**'
      - 'Cargo.toml'
      - 'Cargo.lock'
      - '.github/workflows/core.yml'
  pull_request:

jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: clippy, rustfmt, llvm-tools-preview
      - uses: taiki-e/install-action@cargo-llvm-cov
      - name: Format
        run: cargo fmt --all -- --check
      - name: Clippy
        run: cargo clippy --workspace --all-targets --locked -- -D warnings
      - name: Test with coverage gate
        run: cargo llvm-cov --workspace --locked --fail-under-lines 90
      - name: Test single-threaded
        run: cargo test --workspace --locked -- --test-threads=1
```

Both crates are platform-free, so `ubuntu-latest` alone still suffices. The per-OS matrix arrives in Phase 3 with the native crates.

The single-threaded run is new and specific to this phase: `duet-runtime` spawns threads, and a test that only passes under one scheduling regime is a test that will fail mysteriously later.

- [ ] **Step 4: Verify every CI step locally**

Run each and confirm it passes:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo llvm-cov --workspace --locked --fail-under-lines 90
cargo test --workspace --locked -- --test-threads=1
```

- [ ] **Step 5: Commit**

```bash
git add .github/workflows/core.yml crates/
git commit -m "ci: extend gate to the whole workspace and add a single-threaded run"
```

---

## Done criteria

- [ ] `cargo test --workspace` passes — report exact counts per crate
- [ ] `cargo test --workspace -- --test-threads=1` passes with identical counts
- [ ] `cargo llvm-cov --workspace --fail-under-lines 90` exits 0
- [ ] `cargo clippy --workspace --all-targets --locked -- -D warnings` is clean
- [ ] `cargo fmt --all -- --check` is clean
- [ ] `duet-runtime`'s only dependency is `duet-core` — verify with `cargo tree -p duet-runtime`
- [ ] No `unwrap`/`expect` in non-test code
- [ ] No `Mutex` guarding the `Store` anywhere
- [ ] `duet-core` is unchanged by this phase — verify with `git diff --stat main -- crates/duet-core`

## What Phase 2a deliberately does not build

Named so nobody adds them speculatively:

- **A tokio task pool.** Spec §6.2's third context runs user `#[command]` bodies, which arrive with Phase 4's codegen. Building it now means building for a consumer that does not exist.
- **The real `Sink`.** `EventLoopProxy` arrives in Phase 2b with the window layer. The trait is the seam.
- **Async wrappers on `StoreHandle`.** Blocking is correct for microsecond in-process operations, and an async API would force an executor choice this phase cannot justify.
- **Serialization.** Phase 2b picks the codec.
- **Surface lifecycle driving.** `duet-core` has the state machine and policy; wiring them to real surfaces needs the window layer, so it belongs with Phase 2b/3.
- **Batching or coalescing writes.** No benchmark exists yet. Spec §6.4's `Arc<Patch>` note is the first thing to revisit when one does.
