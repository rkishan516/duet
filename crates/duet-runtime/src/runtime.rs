//! The core thread: owns the store, serves requests, delivers notifications.

use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

use duet_core::{Store, SubscriberId, Value};

use crate::command::CoreCommand;
use crate::error::RuntimeError;
use crate::handle::StoreHandle;
use crate::sink::Sink;

thread_local! {
    /// Set for the lifetime of `core_loop`, so `StoreHandle::call` can refuse a
    /// re-entrant request instead of deadlocking on a reply only this thread
    /// can produce. See [`RuntimeError::ReentrantCall`].
    static ON_CORE_THREAD: Cell<bool> = const { Cell::new(false) };
}

/// True when called from the runtime's core thread.
///
/// `StoreHandle::call` consults this before sending a request, so a
/// [`crate::Sink`] implementation that calls back into a `StoreHandle` gets an
/// immediate [`RuntimeError::ReentrantCall`] instead of wedging the thread it
/// is running on.
pub(crate) fn on_core_thread() -> bool {
    ON_CORE_THREAD.with(Cell::get)
}

/// Owns the store on a dedicated thread and serves requests from handles.
///
/// Dropping a `Runtime` drops its own sender. The core thread exits once every
/// [`StoreHandle`] has *also* been dropped; until then it keeps running and
/// keeps the store alive. Use [`Runtime::shutdown`] to stop it deterministically.
#[derive(Debug)]
pub struct Runtime {
    tx: Sender<CoreCommand>,
    join: JoinHandle<()>,
    next_subscriber: Arc<AtomicU64>,
}

impl Runtime {
    /// Starts a core thread owning `root`, delivering notifications to `sink`.
    ///
    /// # Panics
    ///
    /// Panics if the OS refuses to spawn the thread — for example because the
    /// process is out of resources. This is not converted into a `Result`
    /// because a host that cannot start its store thread has no recovery path:
    /// there is nothing meaningful to do with an error here except stop.
    pub fn spawn<S: Sink>(root: Value, sink: S) -> Runtime {
        let (tx, rx) = mpsc::channel();
        let join = thread::Builder::new()
            .name("duet-core".to_string())
            .spawn(move || core_loop(Store::new(root), rx, sink))
            .expect("spawning the core thread should not fail");
        Runtime {
            tx,
            join,
            next_subscriber: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Returns a handle. Call as many times as needed; handles are cheap and
    /// all clones address the same store.
    pub fn handle(&self) -> StoreHandle {
        StoreHandle::new(self.tx.clone(), Arc::clone(&self.next_subscriber))
    }

    /// Allocates a `SubscriberId` that no other caller of this runtime will be
    /// given.
    ///
    /// Ids are caller-supplied in [`duet_core::Store`], and a collision does
    /// not error — it silently delivers one subscriber's notifications to
    /// another. Across a trust boundary (the Flutter surface and the webview
    /// are separate guests) that would be a confidentiality bug, so always
    /// allocate rather than inventing an id.
    pub fn next_subscriber_id(&self) -> SubscriberId {
        SubscriberId(self.next_subscriber.fetch_add(1, Ordering::Relaxed))
    }

    /// Stops the core thread and waits for it to exit.
    ///
    /// Requests already queued ahead of the shutdown request are served first,
    /// so an in-flight write is never lost. Handles outliving this call report
    /// [`RuntimeError::CoreThreadGone`].
    ///
    /// This call is bounded by whatever the slowest in-flight [`crate::Sink::deliver`]
    /// does: the core thread finishes delivering the batch from the write ahead
    /// of `Shutdown` in the queue before it replies. A sink that blocks — which
    /// its contract forbids, but nothing here enforces — makes `shutdown` hang
    /// along with it.
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
///
/// # Panic safety
///
/// If this thread panics — most plausibly inside a caller-supplied
/// [`crate::Sink`] — the request `Receiver` is dropped as the stack unwinds.
/// Every pending and future `StoreHandle` call then fails its `send` or `recv`
/// and reports [`RuntimeError::CoreThreadGone`]. Callers observe an error
/// rather than hanging on a thread that no longer exists. The store's contents
/// are lost with the thread; the runtime does not restart it, because a store
/// that silently reset would be worse than one that reports itself gone.
fn core_loop<S: Sink>(mut store: Store, rx: Receiver<CoreCommand>, sink: S) {
    ON_CORE_THREAD.with(|flag| flag.set(true));
    // `rx` is owned by this function, not borrowed, and that ownership is the
    // anti-hang invariant of the whole crate: returning from this loop (on
    // `Shutdown`, or on panic-driven unwind) drops `rx`, which drops every
    // queued `CoreCommand`, which drops their reply `Sender`s, which wakes
    // every blocked `StoreHandle::call` with a closed-channel error mapped to
    // `RuntimeError::CoreThreadGone` — instead of leaving them waiting forever.
    // `clippy::pedantic` would suggest taking `&Receiver` here; doing so would
    // compile and pass every test today, but would silently reintroduce a hang
    // the next time this thread exits without draining `rx` itself.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sink::{NullSink, RecordingSink};
    use duet_core::{Path, Value};

    fn sample() -> Value {
        Value::map([(
            "editor",
            Value::map([
                ("zoom", Value::Float(1.0)),
                ("theme", Value::Str("dark".into())),
            ]),
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
    fn successive_subscriber_ids_differ() {
        let rt = Runtime::spawn(sample(), NullSink);
        let a = rt.next_subscriber_id();
        let b = rt.next_subscriber_id();
        assert_ne!(a, b, "a fresh id must never repeat the last one issued");
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn subscriber_ids_from_different_handles_of_one_runtime_never_collide() {
        // Two independently created handles must still draw from the same
        // counter: a Flutter surface and a webview surface each get their own
        // StoreHandle, and a collision between them silently cross-delivers
        // one surface's notifications to the other.
        let rt = Runtime::spawn(sample(), NullSink);
        let a = rt.handle().next_subscriber_id();
        let b = rt.handle().next_subscriber_id();
        assert_ne!(a, b);
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn subscriber_id_allocation_works_from_another_thread() {
        let rt = Runtime::spawn(sample(), NullSink);
        let handle = rt.handle();
        let from_other_thread = thread::spawn(move || handle.next_subscriber_id())
            .join()
            .expect("allocating a subscriber id must not panic");
        let from_this_thread = rt.next_subscriber_id();
        assert_ne!(from_other_thread, from_this_thread);
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn set_writes_and_is_visible_to_a_later_get() {
        let rt = Runtime::spawn(sample(), NullSink);
        let handle = rt.handle();
        handle
            .set(&p("editor.zoom"), Value::Float(2.5))
            .expect("set should succeed");
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
        handle
            .set(&p("nope.deeper"), Value::Null)
            .expect_err("writing through a missing key must fail");
        assert_eq!(handle.get(&p("editor.zoom")), Ok(Some(Value::Float(1.0))));
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn handle_is_cloneable_and_clones_share_one_store() {
        let rt = Runtime::spawn(sample(), NullSink);
        let a = rt.handle();
        let b = a.clone();
        a.set(&p("editor.zoom"), Value::Float(9.0))
            .expect("set should succeed");
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
        handle
            .set(&p("editor.zoom"), Value::Float(2.0))
            .expect("set should succeed");
        rt.shutdown().expect("shutdown should succeed");

        let notes = sink.notifications();
        assert_eq!(
            notes.len(),
            1,
            "one overlapping subscription means one notification"
        );
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
        handle
            .set(&p("nope.deeper"), Value::Null)
            .expect_err("writing through a missing key must fail");
        rt.shutdown().expect("shutdown should succeed");

        assert!(
            sink.notifications().is_empty(),
            "a rejected write must produce no notifications"
        );
    }

    #[test]
    fn a_sink_that_calls_back_into_the_store_gets_an_error_not_a_deadlock() {
        use std::sync::{Arc, Mutex};

        // Deliberately re-entrant sink, standing in for an implementation that
        // tries to read the store while marshalling a batch. `deliver` runs on
        // the core thread, so the nested `get` must be refused rather than
        // block that same thread on a reply only it could produce.
        #[derive(Clone)]
        struct ReentrantSink {
            handle: Arc<Mutex<Option<StoreHandle>>>,
            observed: Arc<Mutex<Option<RuntimeError>>>,
        }

        impl Sink for ReentrantSink {
            fn deliver(
                &self,
                _batch: Vec<duet_core::Notification>,
            ) -> Result<(), crate::SinkError> {
                let handle = self
                    .handle
                    .lock()
                    .expect("lock should not be poisoned")
                    .clone()
                    .expect("handle should have been set before delivery");
                let result =
                    handle.get(&Path::parse("editor.zoom").expect("test path should parse"));
                *self.observed.lock().expect("lock should not be poisoned") = result.err();
                Ok(())
            }
        }

        let observed = Arc::new(Mutex::new(None));
        let sink = ReentrantSink {
            handle: Arc::new(Mutex::new(None)),
            observed: observed.clone(),
        };
        let rt = Runtime::spawn(sample(), sink.clone());
        let handle = rt.handle();
        // Hand the sink a handle to call back with, now that the runtime exists.
        *sink.handle.lock().expect("lock should not be poisoned") = Some(handle.clone());

        handle
            .subscribe(duet_core::SubscriberId(1), p("editor.zoom"))
            .expect("subscribe should succeed");
        // The write itself must still succeed: the deadlock, if it happened,
        // would be inside delivery, which runs after the reply is sent.
        handle
            .set(&p("editor.zoom"), Value::Float(3.0))
            .expect("the write itself must succeed even though delivery misbehaves");

        // The runtime must still be usable: a normal call from this thread
        // (which is not the core thread) must succeed.
        assert_eq!(
            handle.get(&p("editor.zoom")),
            Ok(Some(Value::Float(3.0))),
            "the guard must leave the store working, not merely report the problem"
        );
        rt.shutdown().expect("shutdown should succeed");

        assert_eq!(
            *observed.lock().expect("lock should not be poisoned"),
            Some(RuntimeError::ReentrantCall),
            "the nested call from inside deliver must be refused, not hang"
        );
    }

    #[test]
    fn dropping_runtime_keeps_the_core_thread_alive_while_a_handle_survives() {
        let rt = Runtime::spawn(sample(), NullSink);
        let handle = rt.handle();
        drop(rt);

        // The Runtime's own sender is gone, but this handle's clone of it keeps
        // the channel — and so the core thread and the store — alive.
        assert_eq!(
            handle.get(&p("editor.zoom")),
            Ok(Some(Value::Float(1.0))),
            "the core thread must survive as long as any handle does"
        );
    }

    #[test]
    fn subscribe_snapshot_is_the_real_value_at_the_path() {
        // Kills the mutant where core_loop's Subscribe arm replies with an
        // empty snapshot regardless of what the store holds.
        let rt = Runtime::spawn(sample(), NullSink);
        let handle = rt.handle();
        let (_id, snapshot) = handle
            .subscribe(duet_core::SubscriberId(1), p("editor.zoom"))
            .expect("subscribe should succeed");
        assert_eq!(
            snapshot,
            Some(Value::Float(1.0)),
            "the snapshot must be the current value at the subscribed path"
        );
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn unsubscribe_reports_presence_then_absence() {
        // Kills the mutant where core_loop's Unsubscribe arm always replies
        // false, which would make every unsubscribe look like a no-op.
        let rt = Runtime::spawn(sample(), NullSink);
        let handle = rt.handle();
        let (id, _snapshot) = handle
            .subscribe(duet_core::SubscriberId(1), p("editor.zoom"))
            .expect("subscribe should succeed");

        assert_eq!(
            handle.unsubscribe(id),
            Ok(true),
            "removing a live subscription must report it as having been present"
        );
        assert_eq!(
            handle.unsubscribe(id),
            Ok(false),
            "removing the same id twice must report it as no longer present"
        );
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn drop_subscriber_removes_exactly_its_own_subscriptions_and_silences_future_writes() {
        // Kills the mutant where core_loop's DropSubscriber arm always replies
        // 0 — the count this test pins is exactly what teardown relies on to
        // know a surface's subscriptions are really gone.
        let sink = RecordingSink::new();
        let rt = Runtime::spawn(sample(), sink.clone());
        let handle = rt.handle();
        let subscriber = duet_core::SubscriberId(1);
        handle
            .subscribe(subscriber, p("editor.zoom"))
            .expect("subscribe should succeed");
        handle
            .subscribe(subscriber, p("editor.theme"))
            .expect("subscribe should succeed");

        assert_eq!(
            handle.drop_subscriber(subscriber),
            Ok(2),
            "both subscriptions held by this subscriber must be counted and removed"
        );

        handle
            .set(&p("editor.zoom"), Value::Float(5.0))
            .expect("set should succeed");
        rt.shutdown().expect("shutdown should succeed");

        assert!(
            sink.notifications().is_empty(),
            "a write after drop_subscriber must notify nobody: every subscription was removed"
        );
    }

    #[test]
    fn successful_write_matching_no_subscription_delivers_no_batch() {
        // Kills the mutant where core_loop's `if !notifications.is_empty()`
        // guard is removed and every successful write calls sink.deliver, even
        // with an empty batch. Distinct from `sink.notifications()` being
        // empty (which an empty-batch delivery would also satisfy): this
        // asserts on `sink.batches()` directly so a spurious empty delivery is
        // itself the failure.
        let sink = RecordingSink::new();
        let rt = Runtime::spawn(sample(), sink.clone());
        let handle = rt.handle();
        handle
            .set(&p("editor.zoom"), Value::Float(5.0))
            .expect("set should succeed");
        rt.shutdown().expect("shutdown should succeed");

        assert!(
            sink.batches().is_empty(),
            "a write with no overlapping subscription must not call deliver at all"
        );
    }

    #[test]
    fn a_panicking_sink_does_not_hang_the_next_caller() {
        // A sink whose `deliver` always panics, standing in for a
        // catastrophically broken implementation. Rust prints the panic to
        // stderr when the thread unwinds — that output is expected noise from
        // this test, not a sign anything is wrong.
        struct PanickingSink;
        impl Sink for PanickingSink {
            fn deliver(
                &self,
                _batch: Vec<duet_core::Notification>,
            ) -> Result<(), crate::SinkError> {
                panic!("intentional panic: proving a dead core thread surfaces as an error");
            }
        }

        let rt = Runtime::spawn(sample(), PanickingSink);
        let handle = rt.handle();
        handle
            .subscribe(duet_core::SubscriberId(1), p("editor.zoom"))
            .expect("subscribe should succeed");

        // The write itself already replied before delivery runs, so it
        // succeeds even though the sink is about to panic and take the core
        // thread down with it.
        handle
            .set(&p("editor.zoom"), Value::Float(2.0))
            .expect("the write itself must succeed; the panic is inside delivery, which runs after the reply");

        // `deliver` panics unconditionally, synchronously, before core_loop
        // ever loops back to accept another command — so this next call is
        // guaranteed to observe the core thread as gone, not to race it.
        assert_eq!(
            handle.get(&p("editor.zoom")),
            Err(RuntimeError::CoreThreadGone),
            "a caller after a panicking delivery must get an error, not hang forever"
        );
    }

    #[test]
    fn a_closed_sink_does_not_stop_the_core_thread() {
        // A dead UI must not take the store down with it: deliver() reporting
        // SinkError::Closed is the documented non-fatal case, distinct from a
        // panic, and core_loop must keep serving requests afterwards.
        struct ClosedSink;
        impl Sink for ClosedSink {
            fn deliver(
                &self,
                _batch: Vec<duet_core::Notification>,
            ) -> Result<(), crate::SinkError> {
                Err(crate::SinkError::Closed)
            }
        }

        let rt = Runtime::spawn(sample(), ClosedSink);
        let handle = rt.handle();
        handle
            .subscribe(duet_core::SubscriberId(1), p("editor.zoom"))
            .expect("subscribe should succeed");
        handle
            .set(&p("editor.zoom"), Value::Float(2.0))
            .expect("set should succeed");

        assert_eq!(
            handle.get(&p("editor.zoom")),
            Ok(Some(Value::Float(2.0))),
            "the core thread must keep serving requests after a Closed delivery"
        );
        rt.shutdown().expect("shutdown should succeed");
    }
}
