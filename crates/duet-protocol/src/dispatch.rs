//! Serving a guest request against the store.

use duet_core::SubscriberId;
use duet_runtime::StoreHandle;

use crate::command::{CommandHost, NoCommands};
use crate::message::{Request, RequestId, Response};

/// Serves one guest request against the store, with no commands registered.
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
///
/// # Commands
///
/// This entry point predates [`Request::Invoke`] and registers no commands, so
/// an `invoke` is answered with a [`Response::Failed`] naming the command — see
/// [`crate::NoCommands`]. Every other request kind behaves exactly as it always
/// has; `dispatch_answers_identically_with_and_without_a_command_host` pins
/// that, reply for reply. A host that wants commands calls [`dispatch_with`]
/// instead.
pub fn dispatch(store: &StoreHandle, subscriber: SubscriberId, request: Request) -> Response {
    dispatch_with(store, subscriber, &NoCommands, request)
}

/// Serves one guest request against the store and `commands`.
///
/// Identical to [`dispatch`] for every request kind except [`Request::Invoke`],
/// which is routed into `commands`.
///
/// # `commands` is per-surface, and that is the authorization boundary
///
/// Which commands a guest may reach is decided entirely by which
/// [`CommandHost`] the caller passes here. An `invoke` carries no caller
/// identity to check, because there is nothing for a guest to assert: a webview
/// running untrusted content simply has no name for a command its own surface's
/// host does not hold. Two surfaces sharing one store may be built with two
/// different command hosts, exactly as they are already given two different
/// `SubscriberId`s.
///
/// # Total, including against a command body that is not
///
/// A command body is arbitrary user code, so this is the one path here that
/// could fail in ways `duet-protocol` does not control. Both hazards are handled
/// at a single choke point rather than asked of each implementor — a panic
/// becomes a [`Response::Failed`], and a return too deep for any envelope to
/// carry becomes one too. See [`crate::command`] for both, and
/// [`CommandHost::invoke`] for which thread a body runs on and what it may do
/// there.
pub fn dispatch_with(
    store: &StoreHandle,
    subscriber: SubscriberId,
    commands: &dyn CommandHost,
    request: Request,
) -> Response {
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
        // The store is handed through so a body need not capture a handle of
        // its own; `subscriber` deliberately is *not*. A command is host code
        // running with the host's authority, not a guest's — giving it the
        // caller's subscriber id would invite exactly the "act on behalf of
        // whoever asked" pattern that `Request::Subscribe` carrying no
        // subscriber exists to prevent.
        Request::Invoke { command, args, .. } => {
            crate::command::run(commands, store, id, &command, args)
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
    fn dispatch_answers_identically_with_and_without_a_command_host() {
        // `dispatch` became a delegation to `dispatch_with` when commands were
        // added. That is a real change to a function three transports call, and
        // the claim it has to earn is that a host with no commands behaves
        // exactly as before. Reply equality across every non-command request
        // kind is that claim, checked rather than asserted in a comment.
        //
        // `Response` derives `PartialEq`, so this compares the whole reply —
        // id, kind and payload — not just the discriminant.
        let rt = rt();
        let handle = rt.handle();
        let subscription = match dispatch(
            &handle,
            SubscriberId(1),
            Request::Subscribe {
                id: RequestId(30),
                path: Path::root(),
            },
        ) {
            Response::Subscribed { subscription, .. } => subscription,
            other => panic!("expected Subscribed, got {other:?}"),
        };

        for request in [
            Request::Get {
                id: RequestId(31),
                path: p("editor.zoom"),
            },
            Request::Get {
                id: RequestId(32),
                path: p("editor.absent"),
            },
            Request::Set {
                id: RequestId(33),
                path: p("editor.zoom"),
                value: Value::Float(2.0),
            },
            Request::Set {
                id: RequestId(34),
                path: p("nope.deeper"),
                value: Value::Null,
            },
            Request::Unsubscribe {
                id: RequestId(35),
                subscription,
            },
        ] {
            let without = dispatch(&handle, SubscriberId(1), request.clone());
            let with = dispatch_with(&handle, SubscriberId(1), &NoCommands, request.clone());
            assert_eq!(
                without, with,
                "{request:?} must answer identically through both entry points"
            );
        }
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn an_invoke_with_no_commands_registered_is_failed_and_names_the_command() {
        // What every host shipping today does with an `invoke`: the request
        // decodes fine — it is a legal message — and is then refused because
        // nothing is registered to serve it. Explicitly NOT an unknown-kind
        // error, which would send a developer to debug an encoder that is
        // working correctly.
        let rt = rt();
        let response = dispatch(
            &rt.handle(),
            SubscriberId(1),
            Request::Invoke {
                id: RequestId(40),
                command: "add".to_string(),
                args: crate::Args::new(),
            },
        );
        match response {
            Response::Failed { id, message } => {
                assert_eq!(id, RequestId(40), "the guest must fail that one call");
                assert!(message.contains("add"), "got {message}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn dispatch_with_routes_an_invoke_into_the_command_host_and_nothing_else() {
        // Two properties in one drive: an `invoke` reaches the host with its
        // command name and arguments intact, and a `get` does not reach it at
        // all. A `dispatch_with` that consulted the command host for every
        // request would pass a test that only checked the first.
        use crate::{CommandHost, Outcome};
        use std::cell::RefCell;

        struct Recording {
            seen: RefCell<Vec<String>>,
        }

        impl CommandHost for Recording {
            fn invoke(&self, command: &str, args: crate::Args, _: &StoreHandle) -> Outcome {
                self.seen.borrow_mut().push(command.to_string());
                match args.get("a") {
                    Some(Value::Int(n)) => Outcome::Returned(Value::Int(n + 1)),
                    _ => Outcome::Refused("expected an int argument \"a\"".to_string()),
                }
            }
        }

        let rt = rt();
        let handle = rt.handle();
        let host = Recording {
            seen: RefCell::new(Vec::new()),
        };

        assert_eq!(
            dispatch_with(
                &handle,
                SubscriberId(1),
                &host,
                Request::Invoke {
                    id: RequestId(41),
                    command: "add".to_string(),
                    args: crate::Args::from([("a".to_string(), Value::Int(2))]),
                },
            ),
            Response::Returned {
                id: RequestId(41),
                value: Value::Int(3)
            }
        );

        dispatch_with(
            &handle,
            SubscriberId(1),
            &host,
            Request::Get {
                id: RequestId(42),
                path: p("editor.zoom"),
            },
        );

        assert_eq!(
            host.seen.borrow().as_slice(),
            ["add"],
            "only the invoke may reach the command host"
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
