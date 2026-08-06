//! The messages guests and the host exchange.

use std::collections::BTreeMap;

use duet_core::{Notification, Path, SubscriptionId, Value};

/// A command's named arguments. Always a map: a command's parameter list is a
/// struct's field list, and [`Value::Map`] is the only shape either guest emits.
///
/// # Why this is a `BTreeMap` and not a [`Value`]
///
/// Typed as a `Value`, `"args":{"t":"i","v":"1"}` would decode perfectly well
/// and then fail two layers later — inside a command body that expected to look
/// up a field on something that is not a map, with no request id in scope and
/// no natural place to report it. Narrowing at the decoder instead makes the
/// illegal state unrepresentable: by the time an [`Args`] exists, it is a map.
///
/// The wire still carries a *tagged* value (`{"t":"m","v":{…}}`), because that
/// path is already total against hostile input and re-deriving it here would be
/// a second decoder to keep correct. [`crate::decode_request`] decodes the
/// tagged value first and then narrows, reporting `bad_shape` if it is anything
/// but a map.
///
/// A `BTreeMap` specifically, matching [`Value::Map`]: key order is then
/// deterministic, which is what keeps an encoded `invoke` byte-stable across
/// runs and languages.
pub type Args = BTreeMap<String, Value>;

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

impl RequestId {
    /// The id a [`Response::Failed`] carries when the host could not read the
    /// id of the request it is refusing.
    ///
    /// Not a correlation id: it means *"this reply answers no request you can
    /// name"*. The host emits it when the message was unparseable, nested past
    /// the wire's limit, or carried an id outside the canonical spelling or the
    /// wire's domain — see [`crate::text`]. Guessing an id it cannot trust would
    /// be worse, because a reply naming an id the guest never sent goes
    /// unmatched *and* consumes the guest's chance to notice.
    ///
    /// # Every guest must handle a reply carrying this
    ///
    /// A guest keying pending requests by the id it sent will find nothing for
    /// this one. Dropping it there leaves that request's promise or completer
    /// unsettled forever — a hang with no error, which is the failure shape this
    /// project has now found three times. A guest must surface it instead: see
    /// `onResponse` in `crates/duet-webview/src/bootstrap.rs` and
    /// `WryTransport` in `packages/duet-js/src/wry.ts` for the two
    /// implementations that hold a pending map.
    ///
    /// `0` is a legal request id in its own right, and the wire cannot tell the
    /// two apart. It does not have to: every Duet client allocates ids from 1
    /// upward precisely so that `0` is never one of its own, which makes an
    /// unmatched `0` unambiguous *to the guest* even though it is ambiguous on
    /// the wire.
    pub const UNCORRELATED: RequestId = RequestId(0);
}

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
    /// Run a host command.
    ///
    /// Deliberately carries **no** caller identity — no `subscriber`, no
    /// `surface` — for the same reason [`Request::Subscribe`] carries no
    /// `SubscriberId`. Which commands a guest may reach is decided by which
    /// [`crate::CommandHost`] its surface was built with, never by the guest.
    /// A guest that could name its own caller identity could name another
    /// guest's, and authorization decided by the party being authorized is not
    /// authorization.
    Invoke {
        /// Correlation id.
        id: RequestId,
        /// Which command to run. Guest-chosen, and therefore untrusted: bound
        /// it with [`duet_core::truncated`] before putting it in any message.
        command: String,
        /// The command's named arguments.
        args: Args,
    },
}

impl Request {
    /// The correlation id this request expects echoed back.
    pub fn id(&self) -> RequestId {
        match self {
            Request::Get { id, .. }
            | Request::Set { id, .. }
            | Request::Subscribe { id, .. }
            | Request::Unsubscribe { id, .. }
            | Request::Invoke { id, .. } => *id,
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
    ///
    /// For an [`Request::Invoke`] this means the host **refused**: there is no
    /// command by that name, the arguments did not decode, or the body panicked.
    /// It never means the command ran and returned an error — that is
    /// [`Response::Raised`].
    Failed {
        /// The request this answers.
        id: RequestId,
        /// Why it failed.
        message: String,
    },
    /// The command ran and returned.
    ///
    /// Always a tagged value; a command returning nothing sends `{"t":"n"}`,
    /// i.e. [`Value::Null`]. There is no absent case here, which is why this
    /// carries a `Value` and not an `Option<Value>`: JSON `null` already means
    /// *"the path is absent"* everywhere else in this format — see
    /// [`Response::Value`] — and spending that distinction twice, on two
    /// different questions, is how a format ends up with a value nobody can
    /// interpret without knowing which field they are looking at.
    Returned {
        /// The request this answers.
        id: RequestId,
        /// What the command returned.
        value: Value,
    },
    /// The command returned `Err(E)` — a **domain** outcome, decoded typed by
    /// the guest, not a refusal.
    ///
    /// # Why this is not a [`Response::Failed`]
    ///
    /// `failed` carries a `String`, and flattening a developer's typed error
    /// into prose is not reversible: a guest that wanted to match on
    /// `InsufficientFunds { short_by }` gets a sentence to regex instead. The
    /// two are also different *events* — `failed` says the call did not happen,
    /// `raised` says it happened and the answer was a failure — and a guest
    /// that cannot tell them apart cannot decide whether retrying is safe.
    Raised {
        /// The request this answers.
        id: RequestId,
        /// The error the command returned, tagged like any other value.
        error: Value,
    },
}

impl Response {
    /// The request this response answers.
    pub fn id(&self) -> RequestId {
        match self {
            Response::Value { id, .. }
            | Response::Done { id }
            | Response::Subscribed { id, .. }
            | Response::Failed { id, .. }
            | Response::Returned { id, .. }
            | Response::Raised { id, .. } => *id,
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
        assert_eq!(
            Request::Invoke {
                id: RequestId(11),
                command: "add".to_string(),
                args: Args::new(),
            }
            .id(),
            RequestId(11)
        );
    }

    #[test]
    fn invoke_requests_cannot_name_a_caller() {
        // The same compile-time property `subscribe_requests_cannot_name_a_...`
        // pins for subscriptions, at the point it matters most: a command runs
        // host code, so "which guest is asking" is an authorization question.
        // Duet answers it by construction — a surface reaches exactly the
        // commands its own `CommandHost` holds — and this destructuring stops
        // compiling the moment a `subscriber` or `surface` field appears.
        let r = Request::Invoke {
            id: RequestId(1),
            command: "add".to_string(),
            args: Args::from([("a".to_string(), Value::Int(2))]),
        };
        match r {
            Request::Invoke { id, command, args } => {
                assert_eq!(id, RequestId(1));
                assert_eq!(command, "add");
                assert_eq!(args.get("a"), Some(&Value::Int(2)));
            }
            other => panic!("expected Invoke, got {other:?}"),
        }
    }

    #[test]
    fn args_are_a_map_and_cannot_be_any_other_shape() {
        // `Args` is a `BTreeMap`, not a `Value`. If it were a `Value`, this
        // would be a legal `Args` — and every command body would have to
        // re-check it. The assertion is that the type does not admit the
        // value: `Args::from` below only accepts key/value pairs, so making
        // this line compile with a scalar is impossible.
        let args: Args = Args::from([
            ("b".to_string(), Value::Int(1)),
            ("a".to_string(), Value::Int(2)),
        ]);
        // And the ordering is deterministic, which is what keeps an encoded
        // `invoke` byte-stable: `b` was inserted first and still sorts second.
        assert_eq!(
            args.keys().collect::<Vec<_>>(),
            vec!["a", "b"],
            "args must iterate in key order"
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
        assert_eq!(
            Response::Returned {
                id: RequestId(10),
                value: Value::Int(5)
            }
            .id(),
            RequestId(10)
        );
        assert_eq!(
            Response::Raised {
                id: RequestId(11),
                error: Value::Str("store".into())
            }
            .id(),
            RequestId(11)
        );
    }

    #[test]
    fn a_command_returning_nothing_is_null_the_value_not_null_the_absence() {
        // The distinction `Returned` deliberately does not spend: a unit return
        // is `Value::Null`, which encodes as the tagged `{"t":"n"}`. JSON
        // `null` in this position means nothing at all, and the decoder refuses
        // it — see `wire.rs`. Compare `Response::Value`, where `None` really
        // does mean "the path is absent" and encodes as bare JSON `null`.
        let unit = Response::Returned {
            id: RequestId(1),
            value: Value::Null,
        };
        let absent = Response::Value {
            id: RequestId(1),
            value: None,
        };
        match (unit, absent) {
            (Response::Returned { value, .. }, Response::Value { value: absent, .. }) => {
                assert_eq!(value, Value::Null);
                assert_eq!(absent, None, "the two nulls are different questions");
            }
            other => panic!("expected Returned and Value, got {other:?}"),
        }
    }

    #[test]
    fn a_raised_error_keeps_its_structure_where_failed_would_flatten_it() {
        // The reason `raised` exists as its own kind. A typed error survives as
        // a value a guest can decode back into its own type; `failed` would
        // have reduced it to prose on the way out, and prose does not decode.
        let raised = Response::Raised {
            id: RequestId(1),
            error: Value::map([
                ("code", Value::Str("insufficient_funds".into())),
                ("short_by", Value::Int(250)),
            ]),
        };
        match raised {
            Response::Raised { error, .. } => match error {
                Value::Map(fields) => {
                    assert_eq!(fields.get("short_by"), Some(&Value::Int(250)));
                }
                other => panic!("expected a map, got {other:?}"),
            },
            other => panic!("expected Raised, got {other:?}"),
        }
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
