//! Concurrency, ordering and shutdown guarantees for the runtime, exercised
//! through its public API only.
//!
//! No test in this file sleeps-then-asserts: every synchronization point is a
//! channel, a `join`, or a bounded wait. A wait that would otherwise hang goes
//! through `bounded`, which fails loudly with a clear message instead of
//! stalling the suite — this crate's entire risk surface is "does anything
//! hang", so a hang here must show up as a fast, diagnosable test failure.

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use duet_core::{Path, SubscriberId, Value};
use duet_runtime::{NullSink, RecordingSink, Runtime, RuntimeError, Sink, SinkError};

/// How long a bounded wait tolerates before declaring a hang. Generous enough
/// to never fire on a loaded CI box; bounded enough that a real deadlock fails
/// the suite in seconds, not by exhausting the CI job's own timeout.
const LIMIT: Duration = Duration::from_secs(30);

/// Runs `f` on a worker thread and fails loudly if it does not finish in time.
///
/// This is a hang *detector*, not a timing assertion: it never sleeps-then-
/// asserts, so it cannot be flaky. The timeout is generous enough never to
/// fire on a loaded CI box, and bounded enough that a deadlock fails the suite
/// instead of stalling it.
fn bounded<T: Send + 'static>(what: &str, f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(LIMIT)
        .unwrap_or_else(|_| panic!("HANG: {what} did not finish within {LIMIT:?}"))
}

/// Stops `rt` through a bounded wait, so a regression that makes `shutdown`
/// hang (for example `core_loop` no longer returning on `Shutdown`) fails this
/// test in `LIMIT` seconds with a clear message, rather than stalling the
/// whole suite past the CI job's own timeout.
fn shutdown_bounded(rt: Runtime) -> Result<(), RuntimeError> {
    bounded("shutdown", move || rt.shutdown())
}

fn root() -> Value {
    Value::map([(
        "editor",
        Value::map([
            ("zoom", Value::Float(1.0)),
            ("theme", Value::Str("dark".into())),
            ("counter", Value::Int(0)),
        ]),
    )])
}

fn p(s: &str) -> Path {
    Path::parse(s).expect("test path should parse")
}

fn counter() -> Path {
    p("editor.counter")
}

#[test]
fn no_lost_updates_under_concurrent_writes() {
    let rt = Runtime::spawn(root(), NullSink);
    let handle = rt.handle();

    // 8 threads x 50 writes each, all racing on the same path. Every write
    // must succeed: the core thread serializes them one at a time, so there
    // is no interleaving that can corrupt or drop one.
    let results = bounded("8 writer threads", move || {
        let writers: Vec<_> = (0..8u32)
            .map(|t| {
                let h = handle.clone();
                thread::spawn(move || {
                    (0..50u32)
                        .map(|i| h.set(&counter(), Value::Int(i64::from(t * 50 + i))).is_ok())
                        .collect::<Vec<bool>>()
                })
            })
            .collect();
        writers
            .into_iter()
            .flat_map(|w| w.join().expect("writer thread should not panic"))
            .collect::<Vec<bool>>()
    });

    assert_eq!(results.len(), 400, "8 threads x 50 writes each");
    assert!(
        results.into_iter().all(|ok| ok),
        "every write must succeed under concurrency"
    );

    shutdown_bounded(rt).expect("shutdown should succeed");
}

#[test]
fn exactly_one_batch_per_successful_write() {
    let sink = RecordingSink::new();
    let rt = Runtime::spawn(root(), sink.clone());
    let handle = rt.handle();
    handle
        .subscribe(SubscriberId(1), counter())
        .expect("subscribe should succeed");

    // 4 threads x 25 writes, one overlapping subscription: every write must
    // deliver exactly one batch holding exactly one notification. None lost,
    // none duplicated, regardless of interleaving.
    bounded("4 writer threads", move || {
        let writers: Vec<_> = (0..4u32)
            .map(|t| {
                let h = handle.clone();
                thread::spawn(move || {
                    for i in 0..25u32 {
                        h.set(&counter(), Value::Int(i64::from(t * 25 + i)))
                            .expect("set should succeed");
                    }
                })
            })
            .collect();
        for w in writers {
            w.join().expect("writer thread should not panic");
        }
    });

    shutdown_bounded(rt).expect("shutdown should succeed");

    let batches = sink.batches();
    assert_eq!(
        batches.len(),
        100,
        "100 writes, each producing exactly one batch"
    );
    assert!(
        batches.iter().all(|b| b.len() == 1),
        "each batch must hold exactly one notification, since exactly one subscription overlaps"
    );
}

#[test]
fn per_writer_ordering_is_preserved() {
    // Ordering is only meaningful per-writer: with several concurrent writers
    // the interleaving of their writes is legitimately non-deterministic (that
    // is what `exactly_one_batch_per_successful_write` exercises). A single
    // writer's own sequence of writes, however, must arrive at the sink in
    // exactly the order it issued them — that is the guarantee the core
    // thread exists to provide.
    let sink = RecordingSink::new();
    let rt = Runtime::spawn(root(), sink.clone());
    let handle = rt.handle();
    handle
        .subscribe(SubscriberId(1), counter())
        .expect("subscribe should succeed");

    for i in 0..100i64 {
        handle
            .set(&counter(), Value::Int(i))
            .expect("set should succeed");
    }

    shutdown_bounded(rt).expect("shutdown should succeed");

    let values: Vec<i64> = sink
        .notifications()
        .into_iter()
        .map(|n| match n.patch.value {
            Value::Int(v) => v,
            other => panic!("expected an Int notification, got {other:?}"),
        })
        .collect();
    let expected: Vec<i64> = (0..100).collect();
    assert_eq!(
        values, expected,
        "a single writer's notifications must be delivered in exactly write order"
    );
}

#[test]
fn shutdown_serves_a_write_queued_ahead_of_it() {
    let sink = RecordingSink::new();
    let rt = Runtime::spawn(root(), sink.clone());
    let handle = rt.handle();
    handle
        .subscribe(SubscriberId(1), counter())
        .expect("subscribe should succeed");

    // This write's full round trip — including its reply — completes strictly
    // before shutdown is even requested. By `Runtime::shutdown`'s own
    // contract ("requests already queued ahead of the shutdown request are
    // served first"), its effects must be durable once shutdown returns: it
    // must land, not be discarded.
    handle
        .set(&counter(), Value::Int(42))
        .expect("set should succeed");

    shutdown_bounded(rt).expect("shutdown should succeed");

    assert_eq!(
        sink.notifications().len(),
        1,
        "the write issued before shutdown must have been delivered, not discarded"
    );
    assert_eq!(
        handle.get(&counter()),
        Err(RuntimeError::CoreThreadGone),
        "calls after shutdown must report the core thread as gone"
    );
}

#[test]
fn a_handle_moved_to_another_thread_still_works() {
    let rt = Runtime::spawn(root(), NullSink);
    let handle = rt.handle();

    // `bounded` itself moves the closure — and so `handle` — onto a worker
    // thread; running the full read-your-write round trip there is exactly
    // "a handle moved to another thread still works", with a hang bound for
    // free.
    let result = bounded("handle used from another thread", move || {
        handle
            .set(&counter(), Value::Int(7))
            .expect("set should succeed");
        handle.get(&counter())
    });

    assert_eq!(result, Ok(Some(Value::Int(7))));
    shutdown_bounded(rt).expect("shutdown should succeed");
}

/// A sink that signals a channel the moment it is dropped.
///
/// `core_loop` owns its `Sink` for its entire life and drops it as the very
/// last act before the thread function returns, so this firing is a reliable
/// proxy for "the core thread has finished its loop and is about to exit".
struct ExitSignalSink {
    on_drop: mpsc::Sender<()>,
}

impl Sink for ExitSignalSink {
    fn deliver(&self, _batch: Vec<duet_core::Notification>) -> Result<(), SinkError> {
        Ok(())
    }
}

impl Drop for ExitSignalSink {
    fn drop(&mut self) {
        let _ = self.on_drop.send(());
    }
}

#[test]
fn dropping_runtime_then_the_last_handle_lets_the_core_thread_exit() {
    let (exit_tx, exit_rx) = mpsc::channel();
    let rt = Runtime::spawn(root(), ExitSignalSink { on_drop: exit_tx });
    let handle = rt.handle();
    drop(rt);

    // The Runtime's own sender is gone, but this handle's clone of it keeps
    // the channel open, so the core thread — and the store — must still be
    // alive and serving requests.
    assert_eq!(handle.get(&counter()), Ok(Some(Value::Int(0))));
    assert!(
        exit_rx.try_recv().is_err(),
        "the core thread must not have exited while a handle survives"
    );

    drop(handle);

    // Every sender is gone now, so the core thread's `rx.recv()` must return
    // `Err`, ending `core_loop` and dropping its `Sink` on the way out — the
    // signal this waits for, bounded so a regression that leaks the thread
    // fails this test instead of hanging it.
    exit_rx.recv_timeout(LIMIT).unwrap_or_else(|_| {
        panic!("HANG: core thread did not exit within {LIMIT:?} of its last handle being dropped")
    });
}
