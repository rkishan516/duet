//! The messages guests and the host exchange.

use duet_core::{Notification, Path, SubscriptionId, Value};

/// Correlates a [`Request`] with the [`Response`] that answers it.
///
/// Guests allocate these. Reusing one before its response arrives makes the
/// pairing ambiguous, so guests should treat them as monotonic.
///
/// Travels as a decimal **string** on the wire: it is a `u64`, and JavaScript
/// numbers are IEEE-754 doubles, so a value above 2^53 would corrupt in the
/// webview while surviving in Dart. `duet-codec` carries `Int` and its ids the
/// same way for the same reason.
///
/// # Invariant: `0..=`[`duet_codec::MAX_WIRE_ID`]
///
/// The inner type is `u64`, but the **wire domain is `i64::MAX`**, because
/// Dart's native `int` is 64-bit signed and cannot parse anything above it.
/// Constructing a larger `RequestId` is possible and is a bug in the
/// constructing code.
///
/// It is not *prevented* here — a validated constructor would make this a
/// fallible newtype on a hot path for a bound reachable only after ~9.2 × 10^18
/// sequential requests. Instead the encoder emits whatever it is given,
/// verbatim, and every decoder in every language refuses an out-of-domain id.
/// A violation therefore fails loudly at the first decode rather than being
/// clamped into a *different* id (answering the wrong request) or accepted by
/// Rust and Dart alike (which is the divergence this bound exists to close).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RequestId(pub u64);

/// Something a guest asks the host to do.
///
/// The variants mirror `duet_runtime::StoreHandle`'s operations one-for-one.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Request {
    /// Read the value at a path.
    Get {
        /// Correlation id.
        id: RequestId,
        /// Path to read.
        path: Path,
    },
    /// Write a value at a path.
    Set {
        /// Correlation id.
        id: RequestId,
        /// Path to write.
        path: Path,
        /// Value to write.
        value: Value,
    },
    /// Watch a path, receiving a snapshot now and [`Push`]es thereafter.
    ///
    /// Deliberately carries **no** `SubscriberId`. The host supplies it from
    /// its own `SurfaceId` mapping — if a guest could name one, the webview
    /// could subscribe as the Flutter surface and receive its notifications,
    /// which crosses the trust boundary between two separate guests.
    Subscribe {
        /// Correlation id.
        id: RequestId,
        /// Path to watch.
        path: Path,
    },
    /// Stop watching.
    Unsubscribe {
        /// Correlation id.
        id: RequestId,
        /// Subscription to cancel.
        subscription: SubscriptionId,
    },
}

impl Request {
    /// The correlation id this request expects echoed back.
    pub fn id(&self) -> RequestId {
        match self {
            Request::Get { id, .. }
            | Request::Set { id, .. }
            | Request::Subscribe { id, .. }
            | Request::Unsubscribe { id, .. } => *id,
        }
    }
}

/// The host's answer to one [`Request`].
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Response {
    /// A value was read. `None` means the path is absent.
    Value {
        /// The request this answers.
        id: RequestId,
        /// The value, or `None` if the path does not exist.
        value: Option<Value>,
    },
    /// The operation succeeded and has nothing to return.
    Done {
        /// The request this answers.
        id: RequestId,
    },
    /// A subscription was registered.
    Subscribed {
        /// The request this answers.
        id: RequestId,
        /// The new subscription's id, for a later `Unsubscribe`.
        subscription: SubscriptionId,
        /// The value at the watched path right now, or `None` if absent.
        snapshot: Option<Value>,
    },
    /// The operation failed. Carries a message safe to show a developer.
    Failed {
        /// The request this answers.
        id: RequestId,
        /// Why it failed.
        message: String,
    },
}

impl Response {
    /// The request this response answers.
    pub fn id(&self) -> RequestId {
        match self {
            Response::Value { id, .. }
            | Response::Done { id }
            | Response::Subscribed { id, .. }
            | Response::Failed { id, .. } => *id,
        }
    }
}

/// Something the host sends a guest without being asked.
///
/// Distinct from [`Response`] because it answers no request: it arrives
/// because something the guest subscribed to changed. Folding it into
/// `Response` would force guests to invent a correlation id for it.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Push {
    /// A store notification for one of this guest's subscriptions.
    Notification(Notification),
}

#[cfg(test)]
mod tests {
    use super::*;
    use duet_core::{Path, Value};

    fn p(s: &str) -> Path {
        Path::parse(s).expect("test path should parse")
    }

    #[test]
    fn requests_carry_their_correlation_id() {
        assert_eq!(
            Request::Get {
                id: RequestId(7),
                path: p("editor.zoom")
            }
            .id(),
            RequestId(7)
        );
        assert_eq!(
            Request::Set {
                id: RequestId(8),
                path: p("a"),
                value: Value::Null
            }
            .id(),
            RequestId(8)
        );
        assert_eq!(
            Request::Subscribe {
                id: RequestId(9),
                path: p("a")
            }
            .id(),
            RequestId(9)
        );
        assert_eq!(
            Request::Unsubscribe {
                id: RequestId(10),
                subscription: duet_core::SubscriptionId(3)
            }
            .id(),
            RequestId(10)
        );
    }

    #[test]
    fn responses_echo_the_id_they_answer() {
        assert_eq!(
            Response::Value {
                id: RequestId(7),
                value: None
            }
            .id(),
            RequestId(7)
        );
        assert_eq!(Response::Done { id: RequestId(8) }.id(), RequestId(8));
        assert_eq!(
            Response::Failed {
                id: RequestId(9),
                message: "nope".to_string()
            }
            .id(),
            RequestId(9)
        );
    }

    #[test]
    fn subscribe_requests_cannot_name_a_subscriber() {
        // Deliberate: the host supplies the SubscriberId from its own
        // SurfaceId mapping. If a guest could choose, the webview could
        // subscribe as the Flutter surface and receive its notifications.
        // This test exists to make that a compile-time property — if a
        // `subscriber` field is ever added, this stops compiling.
        let r = Request::Subscribe {
            id: RequestId(1),
            path: p("a"),
        };
        match r {
            Request::Subscribe { id, path } => {
                assert_eq!(id, RequestId(1));
                assert_eq!(path, p("a"));
            }
            other => panic!("expected Subscribe, got {other:?}"),
        }
    }

    #[test]
    fn a_push_is_not_a_response_and_has_no_request_id() {
        // A notification arrives because something changed, not because the
        // guest asked. Forcing it into Response would make guests invent a
        // fake id to correlate against.
        let push = Push::Notification(duet_core::Notification {
            subscriber: duet_core::SubscriberId(1),
            subscription: duet_core::SubscriptionId(1),
            patch: duet_core::Patch {
                path: p("a"),
                value: Value::Int(1),
            },
        });
        match push {
            Push::Notification(n) => assert_eq!(n.subscription, duet_core::SubscriptionId(1)),
        }
    }
}
