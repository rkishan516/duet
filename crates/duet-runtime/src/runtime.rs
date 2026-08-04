//! The core thread: owns the store, serves requests, delivers notifications.

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
        let _ = handle.set(&p("nope.deeper"), Value::Null);
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
        let _ = handle.set(&p("nope.deeper"), Value::Null);
        rt.shutdown().expect("shutdown should succeed");

        assert!(
            sink.notifications().is_empty(),
            "a rejected write must produce no notifications"
        );
    }
}
