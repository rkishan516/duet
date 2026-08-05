//! A cheap, cloneable, thread-safe handle to the store.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
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
/// forcing an async executor choice on callers. Calling a method from the core
/// thread itself — most plausibly from inside a [`crate::Sink`] implementation
/// — cannot be served, since the core thread would be waiting on a reply only
/// it can produce; such a call returns [`RuntimeError::ReentrantCall`] instead
/// of deadlocking.
#[derive(Debug, Clone)]
pub struct StoreHandle {
    tx: Sender<CoreCommand>,
    next_subscriber: Arc<AtomicU64>,
}

impl StoreHandle {
    /// Wraps a raw command sender as a handle. Crate-private: callers obtain a
    /// `StoreHandle` from [`crate::Runtime::handle`], never by constructing one
    /// directly.
    ///
    /// `next_subscriber` is shared with the owning [`crate::Runtime`] and every
    /// other handle cloned from it, so ids allocated from any of them never
    /// collide.
    pub(crate) fn new(tx: Sender<CoreCommand>, next_subscriber: Arc<AtomicU64>) -> Self {
        StoreHandle {
            tx,
            next_subscriber,
        }
    }

    /// Allocates a `SubscriberId` that no other caller sharing this runtime
    /// will be given.
    ///
    /// Ids are caller-supplied in [`duet_core::Store`], and a collision does
    /// not error — it silently delivers one subscriber's notifications to
    /// another. Across a trust boundary (the Flutter surface and the webview
    /// are separate guests) that would be a confidentiality bug, so always
    /// allocate rather than inventing an id.
    pub fn next_subscriber_id(&self) -> SubscriberId {
        SubscriberId(self.next_subscriber.fetch_add(1, Ordering::Relaxed))
    }

    /// Sends a request and waits for its reply.
    ///
    /// Both a failed send and a closed reply channel mean the core thread is
    /// gone, so both map to the same error. This is the single place that
    /// blocking round-trip is implemented.
    ///
    /// Refuses to serve a call made from the core thread itself: such a call
    /// would block that thread on a reply only it can produce, which is a
    /// permanent, unrecoverable hang rather than an ordinary error. See
    /// [`crate::RuntimeError::ReentrantCall`].
    fn call<T>(&self, make: impl FnOnce(Sender<T>) -> CoreCommand) -> Result<T, RuntimeError> {
        if crate::runtime::on_core_thread() {
            return Err(RuntimeError::ReentrantCall);
        }
        let (reply_tx, reply_rx) = mpsc::channel();
        self.tx
            .send(make(reply_tx))
            .map_err(|_| RuntimeError::CoreThreadGone)?;
        reply_rx.recv().map_err(|_| RuntimeError::CoreThreadGone)
    }

    /// Reads the value at `path`, or `None` if it is absent.
    ///
    /// Returns an owned [`Value`] rather than a reference, because a reference
    /// into the core thread's store cannot cross a thread boundary. The value
    /// is deep-cloned on the core thread, so reading at a broad path —
    /// [`duet_core::Path::root`] above all — copies that whole subtree and
    /// blocks every other reader and writer while it does. Read at the
    /// narrowest path that answers your question.
    ///
    /// # Errors
    ///
    /// [`RuntimeError::CoreThreadGone`] if the runtime has shut down.
    /// [`RuntimeError::ReentrantCall`] if called from the core thread itself.
    pub fn get(&self, path: &Path) -> Result<Option<Value>, RuntimeError> {
        let path = path.clone();
        self.call(|reply| CoreCommand::Get { path, reply })
    }

    /// Writes `value` at `path`.
    ///
    /// Returns once the write has been applied. Notifications for it are
    /// delivered to the [`crate::Sink`] immediately afterwards, on the core
    /// thread, so a slow sink cannot stall the writer. A caller may therefore
    /// observe its own write as complete before subscribers have been
    /// notified. Reads through any `StoreHandle` are unaffected — they are
    /// served after delivery.
    ///
    /// # Errors
    ///
    /// [`RuntimeError::Store`] if the store rejected the write — in which case
    /// nothing was mutated and no notifications were produced.
    /// [`RuntimeError::CoreThreadGone`] if the runtime has shut down.
    /// [`RuntimeError::ReentrantCall`] if called from the core thread itself.
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
    /// [`RuntimeError::ReentrantCall`] if called from the core thread itself.
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

    /// Removes one subscription owned by `subscriber`, returning whether it
    /// was present.
    ///
    /// `subscriber` must be the id of the guest making the request, not the
    /// id recorded on the subscription: passing the latter would satisfy the
    /// ownership check unconditionally and reinstate the cross-guest hijack
    /// that [`duet_core::Store::unsubscribe`] exists to prevent. A `false`
    /// return means *either* that no such subscription exists *or* that it
    /// belongs to another guest — deliberately indistinguishable, so a guest
    /// cannot use this as an oracle for probing which ids are live.
    ///
    /// # Errors
    ///
    /// [`RuntimeError::CoreThreadGone`] if the runtime has shut down.
    /// [`RuntimeError::ReentrantCall`] if called from the core thread itself.
    pub fn unsubscribe(
        &self,
        subscriber: SubscriberId,
        id: SubscriptionId,
    ) -> Result<bool, RuntimeError> {
        self.call(|reply| CoreCommand::Unsubscribe {
            subscriber,
            id,
            reply,
        })
    }

    /// Removes every subscription held by `subscriber`, returning how many.
    ///
    /// Called when a surface goes `Cold`.
    ///
    /// # Errors
    ///
    /// [`RuntimeError::CoreThreadGone`] if the runtime has shut down.
    /// [`RuntimeError::ReentrantCall`] if called from the core thread itself.
    pub fn drop_subscriber(&self, subscriber: SubscriberId) -> Result<usize, RuntimeError> {
        self.call(|reply| CoreCommand::DropSubscriber { subscriber, reply })
    }
}
