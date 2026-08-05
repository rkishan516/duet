//! The macOS Flutter surface: a Dart guest wired to the shared store over a
//! Flutter platform channel.
//!
//! The sibling of [`crate::WebviewSurface`]. Both hand guest text to the same
//! total router, [`duet_protocol::handle_text`], and both encode host pushes
//! with the same [`duet_protocol::push_text`]; only the transport differs. A
//! webview guest speaks `wry`'s IPC and is answered by evaluating JavaScript;
//! a Dart guest speaks a `FlutterBinaryMessenger` channel and is answered by
//! invoking the reply block the engine hands the handler.
//!
//! # The transport, from the real headers
//!
//! `FlutterMacOS.framework/Headers/FlutterBinaryMessenger.h` declares exactly
//! what this module uses:
//!
//! ```text
//! typedef void (^FlutterBinaryReply)(NSData* _Nullable reply);                    // :21
//! typedef void (^FlutterBinaryMessageHandler)(NSData* _Nullable message,
//!                                             FlutterBinaryReply reply);          // :30
//! typedef int64_t FlutterBinaryMessengerConnection;                               // :32
//! - (void)sendOnChannel:(NSString*)channel message:(NSData* _Nullable)message;    // :67
//! - (FlutterBinaryMessengerConnection)setMessageHandlerOnChannel:
//!                                     binaryMessageHandler:                       // :92
//! - (void)cleanUpConnection:(FlutterBinaryMessengerConnection)connection;         // :103
//! ```
//!
//! `makeBackgroundTaskQueue` (`:50-57`) is `@optional` and carries a TODO
//! saying macOS has no background platform channels yet. This module never
//! calls it: everything here runs on the platform thread, which on macOS is
//! the main thread.
//!
//! # What the engine source guarantees about the handler
//!
//! The installed `FlutterMacOS.framework`'s `Info.plist` records a
//! `FlutterEngine` commit hash matching the local engine checkout's HEAD, so
//! these are properties of *this exact binary*, not of the embedder API in
//! general:
//!
//! - The `NSData` handed to the handler is built with
//!   `dataWithBytesNoCopy:…freeWhenDone:NO` — **the bytes are borrowed** from
//!   a buffer the engine owns and may reuse the moment the handler returns.
//!   [`inbound_text`] copies them immediately (`NSData::to_vec`), and no
//!   slice of that `NSData` ever escapes the handler.
//! - That `NSData` is **nil** when the payload is empty, which is why
//!   [`inbound_text`] has a null arm rather than treating null as a bug.
//! - The handler is invoked **inline on the platform thread**; there is no
//!   further thread hop, so a reply sent from inside the handler is already
//!   on the right thread.
//! - `shutDownEngine` does **not** clear the engine's `_messengerHandlers`
//!   map. Teardown must therefore call `cleanUpConnection:` explicitly —
//!   see [`FlutterSurface`]'s `Drop`.
//!
//! # Why one fixed channel name is safe
//!
//! `_messengerHandlers` is per-`FlutterEngine`, and this backend keeps one
//! engine per [`duet_supervisor::SurfaceId`] (see
//! [`crate::backend::MacBackend`] and [`FlutterEngine`]'s one-view-per-engine
//! constraint). Two Flutter surfaces therefore register
//! [`DUET_RPC_CHANNEL`] against two *different* messengers, and traffic
//! cannot cross-wire between them. That is what lets the channel name be a
//! single constant every guest can hard-code, rather than a per-surface name
//! the host would have to communicate out of band.
//!
//! # Panics must not cross the block boundary
//!
//! `block2` 0.6's invoke thunks are `unsafe extern "C-unwind"`, so a Rust
//! panic raised inside the handler unwinds straight into Objective-C frames —
//! which is not survivable. Every path through the handler is therefore
//! wrapped in [`std::panic::catch_unwind`]; see [`FlutterSurface::new`].
//! `objc2`'s mandatory `catch-all` feature (enabled workspace-wide) makes
//! this a live concern rather than a theoretical one: it turns any
//! Objective-C exception into a Rust panic, so `NSData::with_bytes` and the
//! reply block's own invocation are both panic sources.

use std::cell::Cell;
use std::panic::{self, AssertUnwindSafe};
use std::ptr::NonNull;

use block2::{DynBlock, RcBlock};
use duet_core::{Notification, SubscriberId};
use duet_host::BackendError;
use duet_protocol::Push;
use duet_runtime::StoreHandle;
use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_foundation::{NSData, NSString};

use crate::engine::{FlutterEngine, catch_to_backend_error};

/// The one channel the whole protocol travels on, in both directions.
///
/// Must match `kDuetChannel` in `fixtures/duet_guest/lib/duet_client.dart`.
/// The Dart side uses `BasicMessageChannel<String>` with `StringCodec`, which
/// puts the payload on the wire as raw UTF-8 with no envelope and no length
/// prefix — exactly the shape [`duet_protocol::handle_text`] already consumes
/// and produces, and the same primitive
/// [`FlutterEngine::set_lifecycle_state`] already drives for
/// `flutter/lifecycle`.
pub const DUET_RPC_CHANNEL: &str = "duet/rpc";

/// The largest guest request this host will decode, in bytes.
///
/// A guest is a separate renderer whose messages are untrusted. Without a cap
/// it could name an arbitrarily large payload and make the host allocate to
/// match — the copy out of the borrowed `NSData`, then the `String`, then
/// whatever `serde_json` builds on top. The check runs against the `NSData`'s
/// `length` *before* anything is copied, so an oversized request costs the
/// host nothing beyond a fixed-size reply.
///
/// 1 MiB is far above any real request: the largest thing a guest sends is a
/// `set` carrying one `Value`, and `duet-protocol`'s own hostile-input tests
/// treat 1 MB payloads as the pathological case.
const MAX_INBOUND_BYTES: usize = 1024 * 1024;

/// The reply to a request that exceeded [`MAX_INBOUND_BYTES`].
///
/// A `const &str`, not a formatted string: this is emitted on a path that
/// exists precisely because the host is refusing to allocate for this guest.
///
/// The id is `"0"` because the request was never decoded, so its real id is
/// unknown — deliberately, since reading it would mean parsing the very
/// payload being refused. A guest that correlates by id (the Dart client
/// does) sees the mismatch and fails that specific call, which is the
/// intended outcome.
const OVERSIZE_FAILURE: &str = concat!(
    r#"{"kind":"failed","id":"0","#,
    r#""message":"request exceeds the host's inbound size limit"}"#
);

/// The reply sent when serving a request panicked.
///
/// A `const &str` for the same reason as [`OVERSIZE_FAILURE`], and a stronger
/// one: this runs *after* a panic was caught, so the recovery path must not
/// be able to panic itself. No formatting, no allocation, and the caught
/// panic's own message is deliberately not included — building that string
/// is exactly the kind of work that could fail again here, and a guest can do
/// nothing useful with it anyway.
const PANIC_FAILURE: &str = concat!(
    r#"{"kind":"failed","id":"0","#,
    r#""message":"the host failed while serving this request"}"#
);

/// `FlutterBinaryReply` — the block the engine hands the handler to answer
/// one message. See `FlutterBinaryMessenger.h:21`.
type ReplyBlock = DynBlock<dyn Fn(*mut NSData)>;

/// `FlutterBinaryMessageHandler`, owned. See `FlutterBinaryMessenger.h:30`.
///
/// Kept alive for the surface's lifetime: the engine stores the block
/// pointer, so dropping this while the connection is still registered would
/// leave the engine holding a dangling block.
type MessageHandler = RcBlock<dyn Fn(*mut NSData, NonNull<ReplyBlock>)>;

/// A Dart guest that speaks `duet-protocol` to the shared store.
///
/// Its handler holds the surface's own [`SubscriberId`], captured at
/// construction and never read from a message.
/// [`duet_protocol::Request`] carries no subscriber field precisely so that a
/// guest cannot subscribe as another guest — a Flutter surface must not be
/// able to receive a webview surface's notifications, and
/// [`FlutterSurface::push`] enforces the same boundary in the other
/// direction.
///
/// **Main thread only.** Every method here sends Objective-C messages to a
/// `FlutterBinaryMessenger`, which requires the platform thread; this type is
/// `!Send` (both `Retained` and `RcBlock` are), so the compiler prevents it
/// from being moved to another thread, but it cannot check that the *creating*
/// thread was the main one. That remains a caller obligation, as it is for
/// [`FlutterEngine`].
///
/// **Drop before the engine.** See this type's `Drop` for the required order.
pub struct FlutterSurface {
    /// The engine's `FlutterBinaryMessenger`, retained so replies and pushes
    /// have something to send through for as long as this surface lives.
    messenger: Retained<AnyObject>,
    /// The `FlutterBinaryMessengerConnection` (`int64_t`) returned by
    /// `setMessageHandlerOnChannel:binaryMessageHandler:`, needed to undo the
    /// registration in `Drop`.
    connection: i64,
    /// This surface's host-assigned subscriber. The handler captures its own
    /// copy; this one is what [`FlutterSurface::push`] filters against.
    subscriber: SubscriberId,
    /// Keepalive for the registered block — see [`MessageHandler`]. Never
    /// read.
    _handler: MessageHandler,
}

impl FlutterSurface {
    /// Registers the [`DUET_RPC_CHANNEL`] handler against `engine`'s binary
    /// messenger, serving `store` as `subscriber`.
    ///
    /// # Errors
    ///
    /// [`BackendError::Unavailable`] if `engine`'s `binaryMessenger` could
    /// not be reached, or if registering the handler threw an Objective-C
    /// exception (caught by `objc2`'s `catch-all` and converted here rather
    /// than left to unwind).
    pub fn new(
        engine: &FlutterEngine,
        store: StoreHandle,
        subscriber: SubscriberId,
    ) -> Result<Self, BackendError> {
        let messenger = engine.binary_messenger()?;

        // The handler owns everything it needs and borrows nothing from this
        // call — required, since the engine keeps it past `new`. `block2`'s
        // closure bound is `Fn`, so it may only take `&self` on what it
        // captures; `StoreHandle`'s methods and `SubscriberId`'s `Copy` both
        // satisfy that.
        //
        // block2 0.6 puts **no** `Send`/`Sync` bound on a block closure, so
        // the type system does not check what this capture is shared with.
        // It is sound here because the only captured state is a
        // `StoreHandle` (`Send + Sync` — it is a channel sender to the core
        // thread) and a `Copy` id. Anything added to this capture in future
        // must be checked by hand for the same property; the compiler will
        // not do it.
        let handler_store = store;
        let handler: MessageHandler =
            RcBlock::new(move |message: *mut NSData, reply: NonNull<ReplyBlock>| {
                // Exactly one reply per invocation, tracked rather than
                // reasoned about. A `FlutterBinaryReply` carries the engine's
                // response handle and consumes it, so invoking it twice would
                // hand the engine a freed handle — a far worse outcome than
                // the hang it would be papering over. Today no panic can
                // occur between the reply and the end of `serve`, but that is
                // a property of the current body, not of the design; this
                // makes it a property of the design. Fresh per invocation, so
                // the enclosing `Fn` closure stays stateless.
                let replied = Cell::new(false);
                // A panic here would unwind through block2's
                // `extern "C-unwind"` thunk into Objective-C frames, which is
                // not survivable — so the *entire* body runs under
                // `catch_unwind`, not just the parts that look fallible.
                // With `objc2`'s `catch-all` on, every `msg_send!` below
                // (including the one inside `NSData::with_bytes`) is a real
                // panic source, as is anything `handle_text` might do.
                let served = panic::catch_unwind(AssertUnwindSafe(|| {
                    // SAFETY: per `FlutterBinaryMessenger.h:30`, `message` is
                    // a nullable `NSData*` valid for the duration of this
                    // call and `reply` is a live `FlutterBinaryReply` block;
                    // the engine invokes this handler inline on the platform
                    // thread, which is where `serve`'s own contract requires
                    // it to run.
                    unsafe { serve(&handler_store, subscriber, message, reply, &replied) };
                }));
                if served.is_err() && !replied.get() {
                    // The guest is waiting on this reply; leaving it
                    // unanswered would hang that call forever. Wrapped in its
                    // own `catch_unwind` because sending the reply is itself
                    // an Objective-C call that can throw — and a panic
                    // escaping the *recovery* path is no more survivable than
                    // the original one.
                    let _ = panic::catch_unwind(AssertUnwindSafe(|| {
                        // SAFETY: same contract as above — `reply` is still
                        // the live reply block the engine passed in, and
                        // `replied` proves nothing has invoked it yet.
                        unsafe { reply_with(reply, PANIC_FAILURE, &replied) };
                    }));
                }
            });

        let channel = NSString::from_str(DUET_RPC_CHANNEL);
        let messenger_ref = &messenger;
        let handler_ref = &*handler;
        let connection: i64 = catch_to_backend_error(|| {
            // SAFETY: `messenger` was just fetched from a live engine and
            // conforms to `FlutterBinaryMessenger`;
            // `setMessageHandlerOnChannel:binaryMessageHandler:` takes an
            // `NSString*` and a `FlutterBinaryMessageHandler` block and
            // returns `FlutterBinaryMessengerConnection` (`int64_t`), which
            // is the `i64` annotated here — see
            // `FlutterBinaryMessenger.h:32,92`. `channel` and `handler_ref`
            // are both live for the duration of the call, and `handler` is
            // kept alive past it by the returned `FlutterSurface`.
            unsafe {
                msg_send![
                    messenger_ref,
                    setMessageHandlerOnChannel: &*channel,
                    binaryMessageHandler: handler_ref,
                ]
            }
        })?;

        Ok(FlutterSurface {
            messenger,
            connection,
            subscriber,
            _handler: handler,
        })
    }

    /// Delivers a notification to this guest.
    ///
    /// **Filters on the subscriber.** A notification addressed to any other
    /// subscriber is dropped and `Ok(())` is returned. This is the
    /// confidentiality boundary written into the code rather than left to a
    /// caller's discipline: the host drains one notification stream and fans
    /// it out to several surfaces, so a caller that forgets to check would
    /// otherwise leak another guest's state — including paths this guest
    /// never subscribed to and may not be allowed to see. Silently succeeding
    /// (rather than erroring) is deliberate: fan-out to a surface that is not
    /// the addressee is the normal case, not a fault.
    ///
    /// # Errors
    ///
    /// [`BackendError::Unavailable`] if sending threw an Objective-C
    /// exception — for example, a messenger whose engine has already shut
    /// down.
    pub fn push(&self, note: &Notification) -> Result<(), BackendError> {
        if !is_addressed_to(note, self.subscriber) {
            return Ok(());
        }
        let text = duet_protocol::push_text(&Push::Notification(note.clone()));
        let messenger = &self.messenger;
        catch_to_backend_error(|| {
            let channel = NSString::from_str(DUET_RPC_CHANNEL);
            let payload = NSData::with_bytes(text.as_bytes());
            let message: Option<&NSData> = Some(&payload);
            // SAFETY: `messenger` is this surface's own live
            // `FlutterBinaryMessenger`; `sendOnChannel:message:` takes an
            // `NSString*` channel and a nullable `NSData*` message
            // (`FlutterBinaryMessenger.h:67`) and returns `void`, matching
            // the `()` annotation. Both arguments outlive the call.
            unsafe {
                let _: () = msg_send![messenger, sendOnChannel: &*channel, message: message];
            }
        })
    }

    /// This surface's host-assigned subscriber — the one its handler serves
    /// and the one [`FlutterSurface::push`] filters against.
    pub fn subscriber(&self) -> SubscriberId {
        self.subscriber
    }
}

impl Drop for FlutterSurface {
    /// Unregisters the channel handler.
    ///
    /// **This is not optional, and the order matters.** `shutDownEngine` does
    /// not clear the engine's `_messengerHandlers` map (see the module docs),
    /// so a surface dropped without this would leave the engine holding a
    /// pointer to a block that no longer exists. Drop the `FlutterSurface`
    /// **before** the engine it was built from shuts down:
    ///
    /// ```text
    /// drop(FlutterSurface)   -> cleanUpConnection:   (here)
    /// FlutterEngine::detach  -> flutter/lifecycle, removeFromSuperview
    /// FlutterEngine::shutdown-> shutDownEngine
    /// ```
    ///
    /// Any Objective-C exception is caught and absorbed: `Drop` cannot report
    /// one, there is no alternative action to take, and letting it unwind out
    /// of a destructor is far worse than an unreclaimed handler slot on an
    /// engine that is about to be torn down anyway.
    fn drop(&mut self) {
        let messenger = &self.messenger;
        let connection = self.connection;
        let _ = panic::catch_unwind(AssertUnwindSafe(|| {
            // SAFETY: `messenger` is this surface's own live
            // `FlutterBinaryMessenger` and `connection` is the value its own
            // `setMessageHandlerOnChannel:` returned;
            // `cleanUpConnection:` takes a
            // `FlutterBinaryMessengerConnection` (`int64_t`) and returns
            // `void` (`FlutterBinaryMessenger.h:103`).
            unsafe {
                let _: () = msg_send![messenger, cleanUpConnection: connection];
            }
        }));
    }
}

/// Serves one guest message and answers it through `reply`, recording in
/// `replied` that the reply block was consumed.
///
/// # Safety
///
/// Caller must be on the platform (main) thread. `message` must be null or a
/// valid `NSData*` whose bytes stay valid for the duration of this call, and
/// `reply` must point at a live `FlutterBinaryReply` block that has not yet
/// been invoked.
unsafe fn serve(
    store: &StoreHandle,
    subscriber: SubscriberId,
    message: *mut NSData,
    reply: NonNull<ReplyBlock>,
    replied: &Cell<bool>,
) {
    // SAFETY: delegated to this function's own contract on `message`.
    let Some(text) = (unsafe { inbound_text(message) }) else {
        // SAFETY: `reply` is live per this function's contract and nothing
        // above can have invoked it.
        unsafe { reply_with(reply, OVERSIZE_FAILURE, replied) };
        return;
    };
    // Total by construction: `handle_text` answers malformed guest input with
    // a `failed` response rather than an error, so there is nothing to handle
    // here. `subscriber` is the host-supplied one; a `subscriber` field on
    // the wire is ignored.
    let out = duet_protocol::handle_text(store, subscriber, &text);
    // SAFETY: `reply` is live per this function's contract and has not been
    // invoked on this path.
    unsafe { reply_with(reply, &out, replied) };
}

/// Copies the handler's message into an owned `String`, or returns `None` if
/// it exceeds [`MAX_INBOUND_BYTES`].
///
/// The cap is checked against the `NSData`'s length *before* the copy, so an
/// oversized message is refused without the host ever allocating for it.
///
/// # Safety
///
/// `message` must be null or a valid `NSData*` that stays valid for the
/// duration of this call.
unsafe fn inbound_text(message: *mut NSData) -> Option<String> {
    // A nil `NSData*` is how the engine represents an empty payload, not an
    // error. `handle_text("")` answers it with a `failed` response.
    // SAFETY: per this function's contract `message` is either null — in
    // which case `as_ref` yields `None` — or a valid, live `NSData*`.
    let Some(data) = (unsafe { message.as_ref() }) else {
        return Some(String::new());
    };
    if exceeds_inbound_cap(data.len()) {
        return None;
    }
    // The engine's `NSData` borrows its bytes (`freeWhenDone:NO`), so this
    // copy is mandatory, not an optimization to remove later: no slice of
    // `data` may outlive the handler.
    Some(decode_inbound(&data.to_vec()))
}

/// Invokes `reply` with `text`'s bytes, marking `replied` first.
///
/// `replied` is set **before** the invocation, not after: if the invocation
/// itself fails, the engine's response handle must be assumed spent, and
/// retrying would be the double-reply this flag exists to prevent. Losing one
/// guest call to a hang is the lesser failure.
///
/// # Safety
///
/// Caller must be on the platform (main) thread, and `reply` must point at a
/// live `FlutterBinaryReply` block that has not yet been invoked.
unsafe fn reply_with(reply: NonNull<ReplyBlock>, text: &str, replied: &Cell<bool>) {
    let payload = NSData::with_bytes(text.as_bytes());
    // SAFETY: `reply` points at a live block per this function's contract.
    let reply_block: &ReplyBlock = unsafe { reply.as_ref() };
    replied.set(true);
    // `payload` is alive for the whole call and only dropped after it
    // returns; the engine copies whatever it needs out of the reply before
    // then.
    reply_block.call((Retained::as_ptr(&payload) as *mut NSData,));
}

/// Whether a message of `len` bytes is over [`MAX_INBOUND_BYTES`].
///
/// A free function so the size-cap decision is testable without an engine —
/// everything else on this path needs a live `FlutterBinaryMessenger`.
fn exceeds_inbound_cap(len: usize) -> bool {
    len > MAX_INBOUND_BYTES
}

/// Decodes guest bytes as UTF-8, replacing anything invalid.
///
/// Lossy rather than fallible on purpose: guest bytes are untrusted, and
/// invalid UTF-8 is just one more malformed request. Replacing it lets
/// [`duet_protocol::handle_text`] reject it as bad JSON — a `failed`
/// response the guest can correlate — instead of this layer inventing a
/// second, transport-level error shape for the same situation.
fn decode_inbound(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Whether `note` is addressed to `subscriber`.
///
/// A free function so the confidentiality filter in [`FlutterSurface::push`]
/// is testable without an engine.
fn is_addressed_to(note: &Notification, subscriber: SubscriberId) -> bool {
    note.subscriber == subscriber
}

#[cfg(test)]
mod tests {
    use super::*;
    use duet_core::{Patch, Path, SubscriptionId, Value};
    use duet_protocol::{Response, decode_response};
    use duet_runtime::{NullSink, Runtime};

    // NOTE ON COVERAGE. `FlutterSurface` itself is not tested here and
    // cannot be: `new`, `push` and `Drop` all need a live `FlutterEngine`
    // and the process's main thread, and the default `cargo test` harness
    // gives neither (see `sink.rs`'s ignored test for the same constraint
    // hitting `tao`). Constructing one would also register a handler that
    // nothing could then drive. Tests that merely assert the struct has
    // fields are worse than no test, so there are none. The real proof is
    // the Task 4 example, driven against a booted engine and a Dart guest.
    // What *is* tested below is everything that was deliberately factored
    // out of the handler to be reachable without an engine: the size-cap
    // decision, the inbound decode, the subscriber filter, and the two
    // const replies.

    fn note(subscriber: u64) -> Notification {
        Notification {
            subscriber: SubscriberId(subscriber),
            subscription: SubscriptionId(1),
            patch: Patch {
                path: Path::parse("editor.zoom").expect("test path should parse"),
                value: Value::Float(1.0),
            },
        }
    }

    #[test]
    fn the_size_cap_admits_exactly_the_limit_and_refuses_one_byte_more() {
        // Off-by-one here is the difference between a working 1 MiB request
        // and a guest that can never send one, so the boundary is asserted
        // rather than assumed.
        assert!(!exceeds_inbound_cap(0), "an empty message must be admitted");
        assert!(
            !exceeds_inbound_cap(MAX_INBOUND_BYTES),
            "a message of exactly the cap must be admitted"
        );
        assert!(
            exceeds_inbound_cap(MAX_INBOUND_BYTES + 1),
            "one byte over the cap must be refused"
        );
        assert!(
            exceeds_inbound_cap(usize::MAX),
            "an absurd length must be refused"
        );
    }

    #[test]
    fn invalid_utf8_from_a_guest_is_answered_not_dropped_and_never_panics() {
        // The lossy decode exists so that hostile bytes stay on the normal
        // protocol error path. This drives it into the same router the
        // handler uses, which is the only thing that makes the choice
        // meaningful.
        //
        // The invariant is *answerability*, not failure. Note the third case
        // below: a `0xff` byte inside a JSON string becomes U+FFFD, which is
        // a perfectly legal path segment, so that request decodes and
        // succeeds against a path that happens not to exist — reported as
        // `{"kind":"value","value":null}`, not as an error. That is correct
        // and worth stating: lossy decoding cannot be assumed to turn bad
        // bytes into rejections, only into *something the guest can parse*.
        // A guest that treated "no failure" as "my bytes arrived intact"
        // would be wrong, which is why the Dart client correlates on the
        // echoed id and validates the response shape.
        let rt = Runtime::spawn(Value::map([("editor", Value::Int(0))]), NullSink);
        let cases: [(&[u8], &str); 4] = [
            (&[0xff, 0xfe, 0xfd], "failed"),
            (&[], "failed"),
            (
                b"\xff{\"kind\":\"get\",\"id\":\"1\",\"path\":\"a\"}",
                "failed",
            ),
            (
                b"{\"kind\":\"get\",\"id\":\"1\",\"path\":\"\xff\"}",
                "value",
            ),
        ];
        for (bad, want_kind) in cases {
            let text = decode_inbound(bad);
            let reply = duet_protocol::handle_text(&rt.handle(), SubscriberId(1), &text);
            let json: serde_json::Value = serde_json::from_str(&reply)
                .unwrap_or_else(|e| panic!("reply for {bad:?} must be valid JSON: {e}"));
            decode_response(&json)
                .unwrap_or_else(|e| panic!("reply for {bad:?} must decode as a Response: {e}"));
            assert_eq!(
                json["kind"], want_kind,
                "invalid UTF-8 {bad:?} should be answered with a {want_kind:?} response, got {reply}"
            );
        }
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn valid_utf8_survives_the_inbound_decode_unchanged() {
        // The decode must not mangle a legitimate request, including
        // non-ASCII text inside a path or value.
        let request = r#"{"kind":"get","id":"1","path":"editor.zoom","note":"café ☕"}"#;
        assert_eq!(decode_inbound(request.as_bytes()), request);
    }

    #[test]
    fn push_delivers_only_to_the_subscriber_the_note_names() {
        // The confidentiality boundary. `FlutterSurface::push` cannot run
        // without an engine, so the predicate it is built on is asserted
        // directly.
        assert!(
            is_addressed_to(&note(7), SubscriberId(7)),
            "a note addressed to this surface must be delivered"
        );
        assert!(
            !is_addressed_to(&note(8), SubscriberId(7)),
            "another guest's note must not be delivered to this surface"
        );
        assert!(
            !is_addressed_to(&note(0), SubscriberId(7)),
            "subscriber 0 must not be treated as a wildcard"
        );
    }

    #[test]
    fn the_const_failure_replies_are_responses_a_guest_can_actually_decode() {
        // These two strings are hand-written rather than encoded, precisely
        // so the paths that emit them cannot allocate or panic. That trade
        // means nothing checks their shape at runtime — a typo would reach a
        // guest as an undecodable reply on exactly the paths where the guest
        // is already in trouble. So they are decoded here with the same
        // function a guest uses.
        for (what, text) in [
            ("oversize", OVERSIZE_FAILURE),
            ("panic recovery", PANIC_FAILURE),
        ] {
            let json: serde_json::Value = serde_json::from_str(text)
                .unwrap_or_else(|e| panic!("the {what} reply must be valid JSON: {e}"));
            let decoded = decode_response(&json)
                .unwrap_or_else(|e| panic!("the {what} reply must decode as a Response: {e}"));
            match decoded {
                Response::Failed { id, message } => {
                    assert_eq!(
                        id.0, 0,
                        "the {what} reply answers a request whose id was never read"
                    );
                    assert!(!message.is_empty(), "the {what} reply must say something");
                }
                other => panic!("the {what} reply must be a failure, got {other:?}"),
            }
        }
    }

    #[test]
    fn the_channel_name_matches_the_dart_guest() {
        // `fixtures/duet_guest/lib/duet_client.dart` hard-codes this string
        // in `kDuetChannel`. Nothing links the two at build time, so a
        // rename on either side would silently produce a guest that talks to
        // a channel with no host handler — which surfaces as a null reply,
        // not an error. Pinned here so the Rust side at least fails loudly.
        assert_eq!(DUET_RPC_CHANNEL, "duet/rpc");
    }
}
