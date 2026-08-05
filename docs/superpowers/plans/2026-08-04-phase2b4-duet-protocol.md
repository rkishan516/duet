# Phase 2b-4 — `duet-protocol` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Define the message envelope guests use to talk to the host — requests, responses, and pushed notifications — and the dispatcher that serves them.

**Architecture:** `Request`/`Response`/`Push` mirror `StoreHandle`'s operations one-for-one, encoded with `duet-codec`'s tagged format. A `dispatch` function takes a decoded `Request` plus a `StoreHandle` and returns a `Response`, so serving a guest is a pure function of its message and the store — no transport, no platform, fully testable.

**Tech Stack:** Rust 1.92, edition 2024. Dependencies `duet-core`, `duet-codec`, `duet-runtime`, plus `serde_json` (already `duet-codec`'s dependency).

**Reference:** spec §6.3 (transport). Phase 2b-0 deferred this deliberately: *"the framing is transport-shaped — Tauri IPC and the Flutter platform channel differ — so it belongs with the transport."* Both transports now exist in outline, and they need **identical** framing, so it belongs in one place rather than two.

---

## Background for the implementer

### The shape of the conversation

Two guests — a Flutter renderer and a JavaScript webview — talk to one Rust host. Three message kinds:

| Kind | Direction | Correlated? |
|---|---|---|
| `Request` | guest → host | yes, by `RequestId` |
| `Response` | host → guest | yes, echoes the `RequestId` |
| `Push` | host → guest | no — unsolicited notifications |

`Push` is not a response to anything: it is a store notification arriving because something the guest subscribed to changed. Conflating it with `Response` would force guests to invent a fake request id.

### What already exists

`duet-codec` (merged) encodes the payloads, with a **tagged** format because plain JSON cannot represent `Value` faithfully:

```rust
duet_codec::encode_value(&Value) -> serde_json::Value
duet_codec::decode_value(&serde_json::Value) -> Result<Value, CodecError>
duet_codec::encode_patch / decode_patch
duet_codec::encode_notification / decode_notification
```

Its `Int` values and `u64` ids travel as **decimal strings**, because JavaScript numbers are IEEE-754 doubles and an `i64` above 2^53 would corrupt in the webview while surviving in Dart. **Follow that convention for `RequestId` too** — it is a `u64` and has exactly the same problem.

`duet-runtime` (merged) holds the state:

```rust
StoreHandle::get(&Path) -> Result<Option<Value>, RuntimeError>
StoreHandle::set(&Path, Value) -> Result<(), RuntimeError>
StoreHandle::subscribe(SubscriberId, Path) -> Result<(SubscriptionId, Option<Value>), RuntimeError>
StoreHandle::unsubscribe(SubscriptionId) -> Result<bool, RuntimeError>
```

### Security: this decodes untrusted guest input

Same standard `duet-codec` holds. Every decode path is **total** — malformed bytes produce an error, never a panic, never unbounded allocation. Guest-supplied text echoed into an error message is truncated.

One rule specific to this crate, and it matters more than it looks:

**A guest must never be able to name another guest's `SubscriberId`.** `Request::Subscribe` therefore does **not** carry one. The host supplies it from its own `SurfaceId → SubscriberId` mapping when it calls `dispatch`. If the wire format let a guest choose, the webview could subscribe as the Flutter surface and receive its notifications — a confidentiality breach across a trust boundary. Phase 2a added a `SubscriberId` allocator for the same reason.

---

## Standing quality bar

Every item was a real review finding earlier in this project that cost a round trip.

**Documentation**
- Every public item documented, **including every enum variant and struct field**; `#![deny(missing_docs)]`.
- `# Errors` sections on every `Result` return.
- **Verify doc claims against the code.** Four reviews here found docs stating what the code did not do.

**Tests**
- No tautological assertions; **pin exact counts, not loose bounds.**
- **Close the loop the real system closes.** This project's dominant failure mode — six instances — is a correct test paired with input that cannot fail it. Round-trip every message through **serialized text**, not just through `serde_json::Value`: the real path is Rust → text → guest → text → Rust, and the in-memory hop skips escaping, number formatting and precision entirely. That is exactly how a bug corrupting 30% of `f64` values survived a round-trip test in `duet-codec`.
- Property tests pin structure; example tests pin semantics. Include both.
- Verify each test genuinely fails before the implementation exists.

**Code**
- Functions under 50 lines; `#![forbid(unsafe_code)]`; no `unwrap`/`expect` in non-test code.

---

## File Structure

| File | Responsibility |
|---|---|
| `crates/duet-protocol/Cargo.toml` | Manifest |
| `crates/duet-protocol/src/lib.rs` | Crate docs, module decls, re-exports |
| `crates/duet-protocol/src/message.rs` | `RequestId`, `Request`, `Response`, `Push` |
| `crates/duet-protocol/src/wire.rs` | Encoding and decoding for all three |
| `crates/duet-protocol/src/dispatch.rs` | `dispatch` — serve a `Request` against a `StoreHandle` |
| `crates/duet-protocol/tests/round_trip.rs` | Integration: text round-trips, adversarial input |

---

## Task 1: Scaffold and the message types

**Files:**
- Create: `crates/duet-protocol/{Cargo.toml,src/lib.rs,src/message.rs}`
- Modify: root `Cargo.toml`

- [ ] **Step 1: Add to the workspace**

Extend `members` with `"crates/duet-protocol"`. Leave `exclude = ["spikes"]` alone.

- [ ] **Step 2: Manifest**

```toml
[package]
name = "duet-protocol"
description = "Message envelope between Duet's host and its guests"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[dependencies]
duet-core = { path = "../duet-core" }
duet-codec = { path = "../duet-codec" }
duet-runtime = { path = "../duet-runtime" }
serde_json = { version = "1", features = ["float_roundtrip"] }
```

`float_roundtrip` is **not optional**. Without it `serde_json`'s parser is best-effort rather than correctly-rounded and corrupts roughly 30% of finite `f64` values through a text hop — measured in Phase 2b-0. Cargo features are additive, so `duet-codec` already enabling it would cover this, but declaring it here keeps the requirement visible where a reader will look.

- [ ] **Step 3: Write the failing test**

Create `crates/duet-protocol/src/message.rs`:

```rust
//! The messages guests and the host exchange.

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
```

- [ ] **Step 4: Run test to verify it fails**

Run: `cargo test -p duet-protocol`
Expected: FAIL — `cannot find type Request in this scope`.

- [ ] **Step 5: Write the implementation**

Insert above the test module in `crates/duet-protocol/src/message.rs`:

```rust
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
```

- [ ] **Step 6: Create the crate root**

Create `crates/duet-protocol/src/lib.rs`:

```rust
//! The message envelope between Duet's host and its guests.
//!
//! A Flutter renderer and a JavaScript webview both talk to one Rust host.
//! This crate defines what they say, and [`dispatch`] serves it.
//!
//! # Three message kinds
//!
//! | Kind | Direction | Correlated |
//! |---|---|---|
//! | [`Request`] | guest → host | by [`RequestId`] |
//! | [`Response`] | host → guest | echoes the id |
//! | [`Push`] | host → guest | no |
//!
//! [`Push`] is separate because it answers nothing — it arrives because
//! something the guest subscribed to changed.
//!
//! # Untrusted input
//!
//! Guests are separate processes and their messages are untrusted. Every decode
//! path is total: malformed bytes produce an error, never a panic. And
//! [`Request::Subscribe`] deliberately carries no `SubscriberId` — the host
//! supplies it, so one guest cannot subscribe as another.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod message;

pub use message::{Push, Request, RequestId, Response};
```

The crate docs reference `dispatch`, which arrives in Task 3. **If rustdoc warns about the broken intra-doc link, demote it to plain backticks** and convert it then. Report which you did.

- [ ] **Step 7: Run and commit**

Run: `cargo test -p duet-protocol`
Expected: PASS — 4 passed.

```bash
git add Cargo.toml Cargo.lock crates/duet-protocol/
git commit -m "feat(protocol): add Request, Response and Push"
```

---

## Task 2: Encoding

**Files:**
- Create: `crates/duet-protocol/src/wire.rs`
- Modify: `crates/duet-protocol/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/duet-protocol/src/wire.rs`:

```rust
//! Encoding and decoding for the message envelope.

#[cfg(test)]
mod tests {
    use super::*;
    use duet_core::{Path, SubscriptionId, Value};

    fn json(s: &str) -> serde_json::Value {
        serde_json::from_str(s).expect("test JSON should parse")
    }

    fn p(s: &str) -> Path {
        Path::parse(s).expect("test path should parse")
    }

    #[test]
    fn a_get_request_encodes_with_its_kind_and_id() {
        assert_eq!(
            encode_request(&Request::Get {
                id: RequestId(7),
                path: p("editor.zoom")
            }),
            json(r#"{"kind":"get","id":"7","path":"editor.zoom"}"#)
        );
    }

    #[test]
    fn request_ids_travel_as_strings_so_both_guests_agree() {
        // u64 exceeds JavaScript's safe integer range exactly as i64 does.
        let big = RequestId(u64::MAX);
        let encoded = encode_request(&Request::Get {
            id: big,
            path: p("a"),
        });
        assert_eq!(encoded["id"], json(r#""18446744073709551615""#));
        assert_eq!(
            decode_request(&encoded).expect("decodes").id(),
            big,
            "the id must survive intact"
        );
    }

    #[test]
    fn every_request_variant_round_trips() {
        for original in [
            Request::Get {
                id: RequestId(1),
                path: p("a.b"),
            },
            Request::Set {
                id: RequestId(2),
                path: p("a[0]"),
                value: Value::Bytes(vec![1, 2, 3]),
            },
            Request::Subscribe {
                id: RequestId(3),
                path: Path::root(),
            },
            Request::Unsubscribe {
                id: RequestId(4),
                subscription: SubscriptionId(u64::MAX),
            },
        ] {
            let decoded = decode_request(&encode_request(&original)).expect("decodes");
            assert_eq!(decoded, original);
        }
    }

    #[test]
    fn every_response_variant_round_trips() {
        for original in [
            Response::Value {
                id: RequestId(1),
                value: Some(Value::Float(1.5)),
            },
            Response::Value {
                id: RequestId(2),
                value: None,
            },
            Response::Done { id: RequestId(3) },
            Response::Subscribed {
                id: RequestId(4),
                subscription: SubscriptionId(9),
                snapshot: Some(Value::Str("x".into())),
            },
            Response::Failed {
                id: RequestId(5),
                message: "boom".to_string(),
            },
        ] {
            let decoded = decode_response(&encode_response(&original)).expect("decodes");
            assert_eq!(decoded, original);
        }
    }

    #[test]
    fn a_push_round_trips() {
        let original = Push::Notification(duet_core::Notification {
            subscriber: duet_core::SubscriberId(1),
            subscription: SubscriptionId(2),
            patch: duet_core::Patch {
                path: p("a"),
                value: Value::Bool(true),
            },
        });
        assert_eq!(decode_push(&encode_push(&original)).expect("decodes"), original);
    }

    #[test]
    fn decode_rejects_malformed_messages_without_panicking() {
        for bad in [
            r#"42"#,
            r#"{}"#,
            r#"{"kind":"nope","id":"1"}"#,
            r#"{"kind":"get"}"#,
            r#"{"kind":"get","id":1,"path":"a"}"#,
            r#"{"kind":"get","id":"x","path":"a"}"#,
            r#"{"kind":"get","id":"1","path":"a.[0]"}"#,
            r#"{"kind":"set","id":"1","path":"a"}"#,
            r#"{"kind":"unsubscribe","id":"1","subscription":"x"}"#,
        ] {
            let parsed = json(bad);
            assert!(
                decode_request(&parsed).is_err(),
                "{bad} must be rejected, got {:?}",
                decode_request(&parsed)
            );
        }
    }

    #[test]
    fn guest_supplied_text_is_bounded_in_error_messages() {
        // This decodes untrusted input; an unbounded echo turns a 1 MB payload
        // into a 1 MB log line.
        let huge = "z".repeat(10_000);
        let bad = json(&format!(r#"{{"kind":"{huge}","id":"1"}}"#));
        let rendered = decode_request(&bad).expect_err("must reject").to_string();
        assert!(
            rendered.len() < 300,
            "error message must be bounded, got {} chars",
            rendered.len()
        );
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p duet-protocol`
Expected: FAIL — `cannot find function encode_request in this scope`.

- [ ] **Step 3: Write the implementation**

Insert above the test module in `crates/duet-protocol/src/wire.rs`:

```rust
use duet_codec::CodecError;
use duet_core::SubscriptionId;
use serde_json::{Map as JsonMap, Value as Json};

use crate::message::{Push, Request, RequestId, Response};

/// Builds an object with a `kind` discriminator.
fn tagged(kind: &str, id: RequestId) -> JsonMap<String, Json> {
    let mut m = JsonMap::new();
    m.insert("kind".to_string(), Json::String(kind.to_string()));
    // A decimal string: `u64` exceeds JavaScript's safe integer range.
    m.insert("id".to_string(), Json::String(id.0.to_string()));
    m
}

fn field<'a>(obj: &'a JsonMap<String, Json>, name: &str) -> Result<&'a Json, CodecError> {
    obj.get(name)
        .ok_or_else(|| CodecError::BadShape(format!("missing \"{name}\"")))
}

fn as_object<'a>(json: &'a Json, what: &str) -> Result<&'a JsonMap<String, Json>, CodecError> {
    json.as_object()
        .ok_or_else(|| CodecError::BadShape(format!("{what} must be an object")))
}

/// Reads a `u64` carried as a decimal string.
fn u64_field(obj: &JsonMap<String, Json>, name: &str) -> Result<u64, CodecError> {
    let s = field(obj, name)?
        .as_str()
        .ok_or_else(|| CodecError::BadShape(format!("\"{name}\" must be a decimal string")))?;
    s.parse::<u64>()
        .map_err(|_| CodecError::BadInt(format!("\"{name}\": {s}")))
}

fn kind<'a>(obj: &'a JsonMap<String, Json>) -> Result<&'a str, CodecError> {
    field(obj, "kind")?
        .as_str()
        .ok_or_else(|| CodecError::BadShape("\"kind\" must be a string".to_string()))
}

/// Encodes a request.
pub(crate) fn encode_request(request: &Request) -> Json {
    let m = match request {
        Request::Get { id, path } => {
            let mut m = tagged("get", *id);
            m.insert("path".to_string(), Json::String(path.to_string()));
            m
        }
        Request::Set { id, path, value } => {
            let mut m = tagged("set", *id);
            m.insert("path".to_string(), Json::String(path.to_string()));
            m.insert("value".to_string(), duet_codec::encode_value(value));
            m
        }
        Request::Subscribe { id, path } => {
            let mut m = tagged("subscribe", *id);
            m.insert("path".to_string(), Json::String(path.to_string()));
            m
        }
        Request::Unsubscribe { id, subscription } => {
            let mut m = tagged("unsubscribe", *id);
            m.insert(
                "subscription".to_string(),
                Json::String(subscription.0.to_string()),
            );
            m
        }
    };
    Json::Object(m)
}

/// Decodes a request.
///
/// # Errors
///
/// A [`CodecError`] describing the first structural problem found. Total over
/// all JSON input: never panics, whatever a guest sends.
pub(crate) fn decode_request(json: &Json) -> Result<Request, CodecError> {
    let obj = as_object(json, "request")?;
    let id = RequestId(u64_field(obj, "id")?);
    let path_of = |obj: &JsonMap<String, Json>| -> Result<duet_core::Path, CodecError> {
        let s = field(obj, "path")?
            .as_str()
            .ok_or_else(|| CodecError::BadShape("\"path\" must be a string".to_string()))?;
        duet_core::Path::parse(s).map_err(|e| CodecError::BadPath(e.to_string()))
    };

    match kind(obj)? {
        "get" => Ok(Request::Get {
            id,
            path: path_of(obj)?,
        }),
        "set" => Ok(Request::Set {
            id,
            path: path_of(obj)?,
            value: duet_codec::decode_value(field(obj, "value")?)?,
        }),
        "subscribe" => Ok(Request::Subscribe {
            id,
            path: path_of(obj)?,
        }),
        "unsubscribe" => Ok(Request::Unsubscribe {
            id,
            subscription: SubscriptionId(u64_field(obj, "subscription")?),
        }),
        other => Err(CodecError::UnknownTag(other.to_string())),
    }
}

/// Encodes a response.
pub(crate) fn encode_response(response: &Response) -> Json {
    let m = match response {
        Response::Value { id, value } => {
            let mut m = tagged("value", *id);
            m.insert(
                "value".to_string(),
                match value {
                    Some(v) => duet_codec::encode_value(v),
                    None => Json::Null,
                },
            );
            m
        }
        Response::Done { id } => tagged("done", *id),
        Response::Subscribed {
            id,
            subscription,
            snapshot,
        } => {
            let mut m = tagged("subscribed", *id);
            m.insert(
                "subscription".to_string(),
                Json::String(subscription.0.to_string()),
            );
            m.insert(
                "snapshot".to_string(),
                match snapshot {
                    Some(v) => duet_codec::encode_value(v),
                    None => Json::Null,
                },
            );
            m
        }
        Response::Failed { id, message } => {
            let mut m = tagged("failed", *id);
            m.insert("message".to_string(), Json::String(message.clone()));
            m
        }
    };
    Json::Object(m)
}

/// Decodes an optional value: JSON `null` means absent.
///
/// Distinct from `Value::Null`, which encodes as `{"t":"n"}` — so an absent
/// path and a path holding null stay distinguishable.
fn optional_value(json: &Json) -> Result<Option<duet_core::Value>, CodecError> {
    if json.is_null() {
        return Ok(None);
    }
    duet_codec::decode_value(json).map(Some)
}

/// Decodes a response.
///
/// # Errors
///
/// A [`CodecError`] describing the first structural problem found.
pub(crate) fn decode_response(json: &Json) -> Result<Response, CodecError> {
    let obj = as_object(json, "response")?;
    let id = RequestId(u64_field(obj, "id")?);

    match kind(obj)? {
        "value" => Ok(Response::Value {
            id,
            value: optional_value(field(obj, "value")?)?,
        }),
        "done" => Ok(Response::Done { id }),
        "subscribed" => Ok(Response::Subscribed {
            id,
            subscription: SubscriptionId(u64_field(obj, "subscription")?),
            snapshot: optional_value(field(obj, "snapshot")?)?,
        }),
        "failed" => Ok(Response::Failed {
            id,
            message: field(obj, "message")?
                .as_str()
                .ok_or_else(|| CodecError::BadShape("\"message\" must be a string".to_string()))?
                .to_string(),
        }),
        other => Err(CodecError::UnknownTag(other.to_string())),
    }
}

/// Encodes a push.
pub(crate) fn encode_push(push: &Push) -> Json {
    match push {
        Push::Notification(n) => {
            let mut m = JsonMap::new();
            m.insert("kind".to_string(), Json::String("notification".to_string()));
            m.insert("notification".to_string(), duet_codec::encode_notification(n));
            Json::Object(m)
        }
    }
}

/// Decodes a push.
///
/// # Errors
///
/// A [`CodecError`] describing the first structural problem found.
pub(crate) fn decode_push(json: &Json) -> Result<Push, CodecError> {
    let obj = as_object(json, "push")?;
    match kind(obj)? {
        "notification" => Ok(Push::Notification(duet_codec::decode_notification(field(
            obj,
            "notification",
        )?)?)),
        other => Err(CodecError::UnknownTag(other.to_string())),
    }
}
```

**Note:** `CodecError`'s `Display` already truncates guest-supplied text — Phase 2b-0 added that after a review found an unbounded echo turning a 1 MB payload into a 1 MB log line. The bounded-message test above relies on it. If it fails, the truncation is not being reached and that is a real finding — report it rather than loosening the assertion.

- [ ] **Step 4: Export from `lib.rs`**

Add `mod wire;` and public wrappers:

```rust
/// Encodes a [`Request`] for transmission.
pub fn encode_request(request: &Request) -> serde_json::Value {
    wire::encode_request(request)
}

/// Decodes a [`Request`] received from a guest.
///
/// # Errors
///
/// A [`duet_codec::CodecError`] describing the first structural problem found.
/// Total over all JSON input: never panics, whatever a guest sends.
pub fn decode_request(json: &serde_json::Value) -> Result<Request, duet_codec::CodecError> {
    wire::decode_request(json)
}
```

Add the equivalent four for `Response` and `Push`.

- [ ] **Step 5: Run and commit**

Run: `cargo test -p duet-protocol`
Expected: PASS — 11 passed.

```bash
git add crates/duet-protocol/src/
git commit -m "feat(protocol): encode requests, responses and pushes"
```

---

## Task 3: `dispatch`

**Files:**
- Create: `crates/duet-protocol/src/dispatch.rs`
- Modify: `crates/duet-protocol/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/duet-protocol/src/dispatch.rs`:

```rust
//! Serving a guest request against the store.

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
            matches!(response, Response::Failed { id: RequestId(3), .. }),
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
            matches!(response, Response::Failed { id: RequestId(10), .. }),
            "got {response:?}"
        );
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p duet-protocol`
Expected: FAIL — `cannot find function dispatch in this scope`.

- [ ] **Step 3: Write the implementation**

Insert above the test module in `crates/duet-protocol/src/dispatch.rs`:

```rust
use duet_core::SubscriberId;
use duet_runtime::StoreHandle;

use crate::message::{Request, RequestId, Response};

/// Serves one guest request against the store.
///
/// `subscriber` is supplied by the **host**, from its own `SurfaceId` mapping —
/// never by the guest. [`Request::Subscribe`] carries no subscriber precisely so
/// that one guest cannot subscribe as another and receive its notifications.
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
        // Answering `Done` regardless of whether the subscription was present
        // keeps this idempotent: a guest retrying after a dropped response must
        // not see a failure for succeeding twice.
        Request::Unsubscribe { subscription, .. } => match store.unsubscribe(subscription) {
            Ok(_) => Response::Done { id },
            Err(e) => failed(id, e),
        },
    }
}

fn failed(id: RequestId, error: duet_runtime::RuntimeError) -> Response {
    Response::Failed {
        id,
        message: error.to_string(),
    }
}
```

- [ ] **Step 4: Export from `lib.rs`**

Add `pub mod dispatch;` and `pub use dispatch::dispatch;`. Convert any plain-backtick `dispatch` reference in the crate docs to an intra-doc link.

- [ ] **Step 5: Run and commit**

Run: `cargo test -p duet-protocol`
Expected: PASS — 19 passed.

```bash
git add crates/duet-protocol/src/
git commit -m "feat(protocol): serve guest requests against the store"
```

---

## Task 4: Text round-trips and adversarial input

**Files:**
- Create: `crates/duet-protocol/tests/round_trip.rs`

- [ ] **Step 1: Write the test**

The in-memory round-trips in Task 2 skip escaping, number formatting and precision. The real path is Rust → **text** → guest → text → Rust, and that is exactly where `duet-codec` had a bug corrupting 30% of `f64` values while its in-memory round-trip passed.

Create `crates/duet-protocol/tests/round_trip.rs`:

```rust
//! Round-trips through serialized text, plus adversarial input.

use duet_core::{Path, SubscriptionId, Value};
use duet_protocol::{
    Push, Request, RequestId, Response, decode_push, decode_request, decode_response, encode_push,
    encode_request, encode_response,
};

fn p(s: &str) -> Path {
    Path::parse(s).expect("test path should parse")
}

/// Values chosen to break a naive encoder: full-precision floats, an `i64`
/// above 2^53, bytes, and strings needing escaping.
fn awkward_values() -> Vec<Value> {
    vec![
        Value::Null,
        Value::Bool(false),
        Value::Int(9_007_199_254_740_993),
        Value::Int(i64::MIN),
        Value::Float(-1.7230175163494897e-48),
        Value::Float(f64::from_bits(0x4021_2345_6789_ABCD)),
        Value::Str("\"quotes\" \\slashes\\ \n newline café 🦀".into()),
        Value::Bytes((0u8..=255).collect()),
        Value::List(vec![Value::Int(1), Value::Null]),
        Value::map([("k", Value::Float(0.1))]),
    ]
}

#[test]
fn every_request_survives_a_text_hop() {
    let mut checked = 0usize;
    for value in awkward_values() {
        for original in [
            Request::Get {
                id: RequestId(u64::MAX),
                path: p("documents[3].title"),
            },
            Request::Set {
                id: RequestId(1),
                path: p("a.b"),
                value: value.clone(),
            },
            Request::Subscribe {
                id: RequestId(2),
                path: Path::root(),
            },
            Request::Unsubscribe {
                id: RequestId(3),
                subscription: SubscriptionId(u64::MAX),
            },
        ] {
            let text = serde_json::to_string(&encode_request(&original)).expect("encodes");
            let reparsed: serde_json::Value = serde_json::from_str(&text).expect("parses");
            let decoded = decode_request(&reparsed)
                .unwrap_or_else(|e| panic!("decode failed for {original:?} via {text}: {e}"));
            assert_eq!(decoded, original, "text round trip changed the request: {text}");
            checked += 1;
        }
    }
    assert_eq!(checked, 40, "enumeration changed; update deliberately");
}

#[test]
fn every_response_survives_a_text_hop() {
    let mut checked = 0usize;
    for value in awkward_values() {
        for original in [
            Response::Value {
                id: RequestId(1),
                value: Some(value.clone()),
            },
            Response::Value {
                id: RequestId(2),
                value: None,
            },
            Response::Done { id: RequestId(3) },
            Response::Subscribed {
                id: RequestId(4),
                subscription: SubscriptionId(7),
                snapshot: Some(value.clone()),
            },
            Response::Failed {
                id: RequestId(5),
                message: "café \"quoted\" 🦀".to_string(),
            },
        ] {
            let text = serde_json::to_string(&encode_response(&original)).expect("encodes");
            let reparsed: serde_json::Value = serde_json::from_str(&text).expect("parses");
            let decoded = decode_response(&reparsed)
                .unwrap_or_else(|e| panic!("decode failed for {original:?} via {text}: {e}"));
            assert_eq!(decoded, original, "text round trip changed the response: {text}");
            checked += 1;
        }
    }
    assert_eq!(checked, 50, "enumeration changed; update deliberately");
}

#[test]
fn a_push_survives_a_text_hop() {
    for value in awkward_values() {
        let original = Push::Notification(duet_core::Notification {
            subscriber: duet_core::SubscriberId(u64::MAX),
            subscription: SubscriptionId(1),
            patch: duet_core::Patch {
                path: p("a[0].b"),
                value,
            },
        });
        let text = serde_json::to_string(&encode_push(&original)).expect("encodes");
        let reparsed: serde_json::Value = serde_json::from_str(&text).expect("parses");
        assert_eq!(decode_push(&reparsed).expect("decodes"), original);
    }
}

#[test]
fn an_absent_value_stays_distinct_from_a_null_value() {
    // `Response::Value { value: None }` means the path does not exist;
    // `Some(Value::Null)` means it exists and holds null. Collapsing them
    // would make "missing" and "explicitly null" indistinguishable to a guest.
    let absent = Response::Value {
        id: RequestId(1),
        value: None,
    };
    let null = Response::Value {
        id: RequestId(1),
        value: Some(Value::Null),
    };
    assert_ne!(encode_response(&absent), encode_response(&null));
    assert_eq!(decode_response(&encode_response(&absent)).expect("decodes"), absent);
    assert_eq!(decode_response(&encode_response(&null)).expect("decodes"), null);
}

#[test]
fn decoding_never_panics_on_arbitrary_json() {
    // The property that matters for a decoder facing untrusted guest input is
    // not that it accepts the right things — it is that it never crashes on the
    // wrong ones.
    const ALPHABET: [char; 8] = ['{', '}', '"', 'k', ':', '1', '[', ']'];
    let mut parsed_ok = 0usize;
    let mut checked = 0usize;

    for len in 0..=5usize {
        for mut code in 0..ALPHABET.len().pow(len as u32) {
            let candidate: String = (0..len)
                .map(|_| {
                    let c = ALPHABET[code % ALPHABET.len()];
                    code /= ALPHABET.len();
                    c
                })
                .collect();
            checked += 1;
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&candidate) {
                parsed_ok += 1;
                let _ = decode_request(&json);
                let _ = decode_response(&json);
                let _ = decode_push(&json);
            }
        }
    }

    // 8^0 + 8^1 + ... + 8^5 = 37449
    assert_eq!(checked, 37_449, "enumeration changed; update deliberately");
    assert!(
        parsed_ok > 0,
        "the alphabet must produce some valid JSON or this test proves nothing"
    );
}
```

- [ ] **Step 2: Run**

Run: `cargo test -p duet-protocol --test round_trip`
Expected: PASS — 5 passed. If a pinned count is wrong, correct it to the real number rather than deleting the assertion, and report the correction.

- [ ] **Step 3: Commit**

```bash
git add crates/duet-protocol/tests/
git commit -m "test(protocol): prove every message survives a text hop"
```

---

## Task 5: Coverage gate and CI

**Files:**
- Modify: `.github/workflows/duet.yml` only if needed

- [ ] **Step 1: Measure**

Run: `cargo llvm-cov -p duet-protocol --summary-only`

`cargo-llvm-cov` 0.8.7 is already installed. It forces an instrumented rebuild taking a few minutes — be patient.

Report real per-file numbers. If any file is below 90%, add tests for those branches. **Do not lower the threshold.** If a line is genuinely unreachable, say so rather than contorting a test.

- [ ] **Step 2: Confirm the workspace gate**

Run: `cargo llvm-cov --workspace --exclude duet-backend-macos --locked --fail-under-lines 90`
Expected: exit 0. Report the total.

The `--exclude duet-backend-macos` is deliberate and already in CI: that crate needs a window server and a Flutter toolchain, neither of which CI has.

- [ ] **Step 3: Verify CI covers the new crate**

Read `.github/workflows/duet.yml`. Every step runs `--workspace --exclude duet-backend-macos`, so the new crate is gated automatically. **Confirm by reading it.** If a step names specific crates instead, fix it. If nothing needs changing, say so — do not manufacture an empty commit.

- [ ] **Step 4: Verify every CI step locally**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --exclude duet-backend-macos --all-targets --locked -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --exclude duet-backend-macos --no-deps --locked
cargo llvm-cov --workspace --exclude duet-backend-macos --locked --fail-under-lines 90
cargo test --workspace --exclude duet-backend-macos --locked -- --test-threads=1
```

- [ ] **Step 5: Commit if anything changed**

```bash
git add .github/workflows/duet.yml crates/ Cargo.lock
git commit -m "ci: gate duet-protocol alongside the rest of the workspace"
```

---

## Done criteria

- [ ] `cargo test --workspace --exclude duet-backend-macos` passes — exact counts per crate
- [ ] Identical counts under `--test-threads=1`
- [ ] `cargo llvm-cov --workspace --exclude duet-backend-macos --fail-under-lines 90` exits 0
- [ ] clippy, fmt and rustdoc clean
- [ ] `duet-protocol` depends only on `duet-core`, `duet-codec`, `duet-runtime` and `serde_json`
- [ ] The other six crates are unchanged — `git diff --stat main -- crates/` shows only `duet-protocol`
- [ ] No `unwrap`/`expect` in non-test code
- [ ] **`Request::Subscribe` carries no `SubscriberId`** — the security property this crate exists to enforce
- [ ] Every message round-trips through **serialized text**, not just in memory

## What Phase 2b-4 deliberately does not build

- **The `wry` IPC wiring and the Flutter platform channel.** Transport-shaped and untestable here; they consume this crate.
- **A TypeScript or Dart client.** Those ship with their transports.
- **Batched or pipelined requests.** No benchmark exists. `Request` is `#[non_exhaustive]`, so a `Batch` variant is additive.
- **Authentication or capability scoping.** Every guest currently sees the whole store. Real scoping needs a policy model this phase has no basis to design — but note that `dispatch` taking the `SubscriberId` from the host is the seam where it would attach.
