//! Where notifications go once the core thread has produced them.

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
    /// Implementations **must not block**. `deliver` is called on the core
    /// thread, which must stay responsive to store requests; a blocking sink
    /// stalls every reader and writer in the process. Marshal and return.
    ///
    /// `deliver` runs on the core thread. Calling any [`crate::StoreHandle`]
    /// method from inside it returns [`crate::RuntimeError::ReentrantCall`] — it
    /// cannot be served, because this thread is the one that would have to
    /// serve it. Marshal the batch elsewhere and return.
    ///
    /// Panic behaviour is **not yet defined** — do not rely on `deliver` being
    /// panic-safe. A later task establishes what happens when it panics; this
    /// is currently left open rather than silently assumed either way.
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
        self.batches
            .lock()
            .expect("recording lock poisoned")
            .clone()
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
        sink.deliver(vec![note(1), note(2)])
            .expect("delivery should succeed");
        sink.deliver(vec![note(3)])
            .expect("delivery should succeed");

        let batches = sink.batches();
        assert_eq!(
            batches.len(),
            2,
            "two deliver calls should record two batches"
        );
        assert_eq!(batches[0].len(), 2);
        assert_eq!(batches[1].len(), 1);
        assert_eq!(batches[0][0].subscriber, SubscriberId(1));
        assert_eq!(batches[1][0].subscriber, SubscriberId(3));
    }

    #[test]
    fn recording_sink_flattens_for_convenience() {
        let sink = RecordingSink::new();
        sink.deliver(vec![note(1), note(2)])
            .expect("delivery should succeed");
        sink.deliver(vec![note(3)])
            .expect("delivery should succeed");

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
    fn clones_share_one_recording() {
        let sink = RecordingSink::new();
        let clone = sink.clone();
        clone
            .deliver(vec![note(1)])
            .expect("delivery should succeed");
        assert_eq!(
            sink.notifications().len(),
            1,
            "a clone and its original must observe the same recording"
        );
    }

    #[test]
    fn empty_batches_are_still_recorded() {
        // A write that matches no subscription produces an empty batch. The
        // core thread may skip delivering it, but the Sink contract must not
        // reject one.
        let sink = RecordingSink::new();
        sink.deliver(Vec::new())
            .expect("empty delivery should succeed");
        assert_eq!(sink.batches().len(), 1);
    }
}
