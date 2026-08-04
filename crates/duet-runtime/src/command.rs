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
/// Constructed by [`crate::StoreHandle`]'s methods and consumed by
/// [`crate::Runtime`]'s core thread loop.
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

    /// Feeds a real `Store`'s return values through the `Subscribe` and
    /// `Unsubscribe` reply channels, so a mismatch between a variant's reply
    /// type and the `Store` method it serves is a compile error, and a
    /// swapped tuple field order is an assertion failure.
    #[test]
    fn reply_types_match_the_store_api_they_serve() {
        use duet_core::{Store, SubscriberId, Value};

        let mut store = Store::new(Value::map([(
            "editor",
            Value::map([("zoom", Value::Float(1.0))]),
        )]));
        let path = Path::parse("editor.zoom").expect("test path should parse");

        // Subscribe: the reply channel must accept exactly what Store::subscribe returns.
        let (tx, rx) = mpsc::channel();
        let cmd = CoreCommand::Subscribe {
            subscriber: SubscriberId(1),
            path: path.clone(),
            reply: tx,
        };
        match cmd {
            CoreCommand::Subscribe {
                subscriber,
                path,
                reply,
            } => {
                reply
                    .send(store.subscribe(subscriber, path))
                    .expect("reply should send");
            }
            other => panic!("expected Subscribe, got {other:?}"),
        }
        let (id, snapshot) = rx.recv().expect("reply should arrive");
        assert_eq!(
            snapshot,
            Some(Value::Float(1.0)),
            "snapshot must be the value at the path"
        );

        // Unsubscribe: bool.
        let (tx, rx) = mpsc::channel();
        let cmd = CoreCommand::Unsubscribe { id, reply: tx };
        match cmd {
            CoreCommand::Unsubscribe { id, reply } => {
                reply
                    .send(store.unsubscribe(id))
                    .expect("reply should send");
            }
            other => panic!("expected Unsubscribe, got {other:?}"),
        }
        assert!(
            rx.recv().expect("reply should arrive"),
            "removing a live subscription returns true"
        );
    }

    /// `Set`'s reply channel carries `Result<(), SetError>`, not
    /// `Store::set`'s own `Result<Vec<Notification>, SetError>` — the
    /// notifications go to the `Sink`, not this channel. This pins the
    /// mapping on the success path: a real write reaches the reply as `Ok(())`.
    #[test]
    fn set_reply_carries_success_from_a_real_write() {
        use duet_core::{Store, Value};

        let mut store = Store::new(Value::map([(
            "editor",
            Value::map([("zoom", Value::Float(1.0))]),
        )]));
        let path = Path::parse("editor.zoom").expect("test path should parse");

        let (tx, rx) = mpsc::channel();
        let cmd = CoreCommand::Set {
            path: path.clone(),
            value: Value::Float(2.0),
            reply: tx,
        };
        match cmd {
            CoreCommand::Set { path, value, reply } => {
                let result = store.set(&path, value).map(|_notifications| ());
                reply.send(result).expect("reply should send");
            }
            other => panic!("expected Set, got {other:?}"),
        }
        assert_eq!(rx.recv().expect("reply should arrive"), Ok(()));
        assert_eq!(
            store.get(&path),
            Some(&Value::Float(2.0)),
            "the write must actually have landed in the store"
        );
    }

    /// Same as above, but for the rejection path: the store's `SetError` must
    /// reach the reply unchanged, not be discarded in favor of `Ok(())`.
    #[test]
    fn set_reply_carries_rejection_from_a_real_write() {
        use duet_core::{Store, Value};

        let mut store = Store::new(Value::map([(
            "editor",
            Value::map([("zoom", Value::Float(1.0))]),
        )]));
        let path = Path::parse("nope.deeper").expect("test path should parse");

        let (tx, rx) = mpsc::channel();
        let cmd = CoreCommand::Set {
            path: path.clone(),
            value: Value::Int(1),
            reply: tx,
        };
        match cmd {
            CoreCommand::Set { path, value, reply } => {
                let result = store.set(&path, value).map(|_notifications| ());
                reply.send(result).expect("reply should send");
            }
            other => panic!("expected Set, got {other:?}"),
        }
        assert_eq!(
            rx.recv().expect("reply should arrive"),
            Err(SetError::MissingKey(path)),
            "the store's rejection must reach the reply unchanged"
        );
    }

    /// `DropSubscriber`'s reply is `usize` — the count of removed
    /// subscriptions from `Store::drop_subscriber`.
    #[test]
    fn drop_subscriber_reply_carries_the_real_removed_count() {
        use duet_core::{Store, SubscriberId, Value};

        let mut store = Store::new(Value::map([(
            "editor",
            Value::map([("zoom", Value::Float(1.0))]),
        )]));
        let subscriber = SubscriberId(7);
        store.subscribe(
            subscriber,
            Path::parse("editor.zoom").expect("test path should parse"),
        );
        store.subscribe(
            subscriber,
            Path::parse("editor").expect("test path should parse"),
        );

        let (tx, rx) = mpsc::channel();
        let cmd = CoreCommand::DropSubscriber {
            subscriber,
            reply: tx,
        };
        match cmd {
            CoreCommand::DropSubscriber { subscriber, reply } => {
                reply
                    .send(store.drop_subscriber(subscriber))
                    .expect("reply should send");
            }
            other => panic!("expected DropSubscriber, got {other:?}"),
        }
        assert_eq!(
            rx.recv().expect("reply should arrive"),
            2,
            "both subscriptions held by the subscriber must be counted"
        );
    }

    /// `Shutdown`'s reply is `()` — a pure signal that carries no data, just
    /// confirmation the core thread has seen the request.
    #[test]
    fn shutdown_reply_signals_with_no_payload() {
        let (tx, rx) = mpsc::channel();
        let cmd = CoreCommand::Shutdown { reply: tx };
        match cmd {
            CoreCommand::Shutdown { reply } => {
                reply.send(()).expect("reply should send");
            }
            other => panic!("expected Shutdown, got {other:?}"),
        }
        assert_eq!(rx.recv().expect("reply should arrive"), ());
    }
}
