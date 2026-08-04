//! The private request type carried from a handle to the core thread.

use std::sync::mpsc::Sender;

use duet_core::{Path, SetError, SubscriberId, SubscriptionId, Value};

/// A request from a `StoreHandle` to the core thread.
///
/// Every variant carries its own reply channel, so a caller waits only on its
/// own request and never on a shared lock. Dropping the reply sender without
/// sending — which happens if the core thread dies mid-request — closes the
/// caller's receiver, and the caller reports
/// [`crate::RuntimeError::CoreThreadGone`] rather than hanging.
///
/// This type is crate-private: it is an implementation detail of how the handle
/// talks to the thread, not part of the public API.
///
/// `#[allow(dead_code)]`: nothing constructs most variants yet, because the
/// core thread that matches on them (`Runtime`/`StoreHandle`) is built in the
/// next task by design — see the phase plan. Remove this allow once that
/// consumer lands.
#[derive(Debug)]
#[allow(dead_code)]
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
                reply
                    .send(Some(Value::Float(1.5)))
                    .expect("reply should send");
            }
            other => panic!("expected Get, got {other:?}"),
        }

        assert_eq!(
            rx.recv().expect("reply should arrive"),
            Some(Value::Float(1.5))
        );
    }

    #[test]
    fn command_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<CoreCommand>();
    }
}
