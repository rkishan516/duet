//! Serving a guest request against the store.

use duet_core::SubscriberId;
use duet_runtime::StoreHandle;

use crate::message::{Request, RequestId, Response};

/// Serves one guest request against the store.
///
/// `subscriber` is supplied by the **host**, from its own `SurfaceId` mapping —
/// never by the guest. [`Request::Subscribe`] carries no subscriber precisely so
/// that one guest cannot subscribe as another and receive its notifications, and
/// [`Request::Unsubscribe`] is scoped to this same `subscriber` so that one
/// guest cannot silence another's subscription by guessing its id.
///
/// Never fails: every error becomes a [`Response::Failed`] carrying a message.
/// A guest that sent a well-formed request always gets a well-formed answer,
/// which is what lets a transport treat this as total.
pub fn dispatch(store: &StoreHandle, subscriber: SubscriberId, request: Request) -> Response {
    let id = request.id();
    match request {
        Request::Get { path, .. } => match store.get(&path) {
            Ok(value) => Response::Value { id, value },
            Err(e) => failed(id, e),
        },
        Request::Set { path, value, .. } => match store.set(&path, value) {
            Ok(()) => Response::Done { id },
            Err(e) => failed(id, e),
        },
        Request::Subscribe { path, .. } => match store.subscribe(subscriber, path) {
            Ok((subscription, snapshot)) => Response::Subscribed {
                id,
                subscription,
                snapshot,
            },
            Err(e) => failed(id, e),
        },
        // Removal is scoped to `subscriber` — the *host-supplied* id, never one
        // the guest names — so a guest cannot remove another guest's
        // subscription by guessing its small sequential `SubscriptionId`.
        //
        // Answering `Done` regardless of what the store reported is
        // deliberate, and load-bearing in two ways. It keeps this idempotent:
        // a guest retrying after a dropped response must not see a failure for
        // succeeding twice. And it keeps the answer uniform across "removed",
        // "never existed", and "belongs to another guest" — reporting `Failed`
        // for a refused removal would hand a guest an oracle for probing which
        // subscription ids are live and which are someone else's. Do not
        // "improve" this by surfacing the store's `bool`.
        Request::Unsubscribe { subscription, .. } => {
            match store.unsubscribe(subscriber, subscription) {
                Ok(_) => Response::Done { id },
                Err(e) => failed(id, e),
            }
        }
    }
}

fn failed(id: RequestId, error: duet_runtime::RuntimeError) -> Response {
    Response::Failed {
        id,
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use duet_core::{Path, SubscriberId, Value};
    use duet_runtime::{NullSink, Runtime};

    fn rt() -> Runtime {
        Runtime::spawn(
            Value::map([("editor", Value::map([("zoom", Value::Float(1.0))]))]),
            NullSink,
        )
    }

    fn p(s: &str) -> Path {
        Path::parse(s).expect("test path should parse")
    }

    #[test]
    fn get_returns_the_value_at_the_path() {
        let rt = rt();
        let response = dispatch(
            &rt.handle(),
            SubscriberId(1),
            Request::Get {
                id: RequestId(1),
                path: p("editor.zoom"),
            },
        );
        assert_eq!(
            response,
            Response::Value {
                id: RequestId(1),
                value: Some(Value::Float(1.0))
            }
        );
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn get_on_a_missing_path_is_a_value_response_with_none() {
        // Absent is not an error — the guest asked a legitimate question.
        let rt = rt();
        let response = dispatch(
            &rt.handle(),
            SubscriberId(1),
            Request::Get {
                id: RequestId(1),
                path: p("editor.nope"),
            },
        );
        assert_eq!(
            response,
            Response::Value {
                id: RequestId(1),
                value: None
            }
        );
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn set_writes_and_answers_done() {
        let rt = rt();
        let handle = rt.handle();
        let response = dispatch(
            &handle,
            SubscriberId(1),
            Request::Set {
                id: RequestId(2),
                path: p("editor.zoom"),
                value: Value::Float(3.0),
            },
        );
        assert_eq!(response, Response::Done { id: RequestId(2) });
        assert_eq!(
            handle.get(&p("editor.zoom")).expect("read should succeed"),
            Some(Value::Float(3.0))
        );
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn a_rejected_write_answers_failed_and_changes_nothing() {
        let rt = rt();
        let handle = rt.handle();
        let response = dispatch(
            &handle,
            SubscriberId(1),
            Request::Set {
                id: RequestId(3),
                path: p("nope.deeper"),
                value: Value::Null,
            },
        );
        assert!(
            matches!(
                response,
                Response::Failed {
                    id: RequestId(3),
                    ..
                }
            ),
            "got {response:?}"
        );
        assert_eq!(
            handle.get(&p("editor.zoom")).expect("read should succeed"),
            Some(Value::Float(1.0)),
            "a rejected write must not mutate"
        );
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn subscribe_answers_with_the_snapshot_and_a_subscription_id() {
        let rt = rt();
        let response = dispatch(
            &rt.handle(),
            SubscriberId(1),
            Request::Subscribe {
                id: RequestId(4),
                path: p("editor.zoom"),
            },
        );
        match response {
            Response::Subscribed { id, snapshot, .. } => {
                assert_eq!(id, RequestId(4));
                assert_eq!(snapshot, Some(Value::Float(1.0)));
            }
            other => panic!("expected Subscribed, got {other:?}"),
        }
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn dispatch_uses_the_subscriber_the_host_supplied_not_one_the_guest_chose() {
        // The security property: a guest names no subscriber, so it cannot
        // subscribe as another guest. Two dispatches with different host-supplied
        // subscribers must produce independent subscriptions.
        let rt = rt();
        let handle = rt.handle();
        let a = dispatch(
            &handle,
            SubscriberId(1),
            Request::Subscribe {
                id: RequestId(5),
                path: Path::root(),
            },
        );
        let b = dispatch(
            &handle,
            SubscriberId(2),
            Request::Subscribe {
                id: RequestId(6),
                path: Path::root(),
            },
        );
        let (sa, sb) = match (a, b) {
            (
                Response::Subscribed {
                    subscription: sa, ..
                },
                Response::Subscribed {
                    subscription: sb, ..
                },
            ) => (sa, sb),
            other => panic!("expected two Subscribed, got {other:?}"),
        };
        assert_ne!(sa, sb);

        // Dropping subscriber 1 must leave subscriber 2's subscription intact.
        assert_eq!(
            handle.drop_subscriber(SubscriberId(1)).expect("drop"),
            1,
            "exactly one subscription belonged to subscriber 1"
        );
        assert_eq!(
            handle.drop_subscriber(SubscriberId(2)).expect("drop"),
            1,
            "subscriber 2's subscription must survive"
        );
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn unsubscribe_answers_done_whether_or_not_it_was_present() {
        // Idempotent by design: a guest retrying an unsubscribe after a dropped
        // response should not see an error for succeeding twice.
        let rt = rt();
        let handle = rt.handle();
        let subscription = match dispatch(
            &handle,
            SubscriberId(1),
            Request::Subscribe {
                id: RequestId(7),
                path: Path::root(),
            },
        ) {
            Response::Subscribed { subscription, .. } => subscription,
            other => panic!("expected Subscribed, got {other:?}"),
        };

        assert_eq!(
            dispatch(
                &handle,
                SubscriberId(1),
                Request::Unsubscribe {
                    id: RequestId(8),
                    subscription
                }
            ),
            Response::Done { id: RequestId(8) }
        );
        assert_eq!(
            dispatch(
                &handle,
                SubscriberId(1),
                Request::Unsubscribe {
                    id: RequestId(9),
                    subscription
                }
            ),
            Response::Done { id: RequestId(9) }
        );
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn one_guest_cannot_unsubscribe_another_guests_subscription() {
        // The layer a guest actually reaches. `SubscriptionId`s are small
        // sequential integers, so guest B needs no information at all to name
        // guest A's subscription — it just guesses. The counterpart of the
        // rule pinned by `dispatch_uses_the_subscriber_the_host_supplied...`:
        // a guest may neither subscribe *as* another guest nor unsubscribe
        // *for* one.
        let rt = rt();
        let handle = rt.handle();
        let victim = SubscriberId(1);
        let attacker = SubscriberId(2);

        let subscription = match dispatch(
            &handle,
            victim,
            Request::Subscribe {
                id: RequestId(20),
                path: Path::root(),
            },
        ) {
            Response::Subscribed { subscription, .. } => subscription,
            other => panic!("expected Subscribed, got {other:?}"),
        };

        // The attacker's attempt is answered `Done` — see
        // `unsubscribe_answers_done_whether_or_not_it_was_present` for why the
        // answer is deliberately uniform — but it must not actually remove
        // anything.
        assert_eq!(
            dispatch(
                &handle,
                attacker,
                Request::Unsubscribe {
                    id: RequestId(21),
                    subscription
                }
            ),
            Response::Done { id: RequestId(21) },
            "a refused unsubscribe must be indistinguishable from a successful one"
        );

        assert_eq!(
            handle.drop_subscriber(victim).expect("drop"),
            1,
            "the victim's subscription must have survived the attacker's guess"
        );

        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn a_guest_can_still_unsubscribe_its_own_subscription() {
        // The positive control for the test above: scoping removal to the owner
        // must not have made removal impossible, which would let that test pass
        // against an implementation that simply never removes anything.
        let rt = rt();
        let handle = rt.handle();
        let subscriber = SubscriberId(1);

        let subscription = match dispatch(
            &handle,
            subscriber,
            Request::Subscribe {
                id: RequestId(22),
                path: Path::root(),
            },
        ) {
            Response::Subscribed { subscription, .. } => subscription,
            other => panic!("expected Subscribed, got {other:?}"),
        };

        assert_eq!(
            dispatch(
                &handle,
                subscriber,
                Request::Unsubscribe {
                    id: RequestId(23),
                    subscription
                }
            ),
            Response::Done { id: RequestId(23) }
        );
        assert_eq!(
            handle.drop_subscriber(subscriber).expect("drop"),
            0,
            "the owner's own unsubscribe must really have removed it"
        );

        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn a_dead_runtime_answers_failed_rather_than_panicking() {
        let rt = rt();
        let handle = rt.handle();
        rt.shutdown().expect("shutdown should succeed");
        let response = dispatch(
            &handle,
            SubscriberId(1),
            Request::Get {
                id: RequestId(10),
                path: p("editor.zoom"),
            },
        );
        assert!(
            matches!(
                response,
                Response::Failed {
                    id: RequestId(10),
                    ..
                }
            ),
            "got {response:?}"
        );
    }
}
