//! The core thread: owns the store, serves requests, delivers notifications.

use std::cell::Cell;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

use duet_core::{Store, Value};

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
}
