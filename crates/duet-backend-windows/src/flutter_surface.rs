//! The Windows Flutter surface: a Dart guest wired to the shared store over a
//! Flutter platform channel.
//!
//! The sibling of [`crate::WebviewSurface`]. Both hand guest text to the same
//! total router, [`duet_protocol::handle_text`], and both encode host pushes
//! with the same [`duet_protocol::push_text`]; only the transport differs. A
//! webview guest speaks `wry`'s IPC and is answered by evaluating JavaScript;
//! a Dart guest speaks a platform channel and is answered through the
//! message's response handle.
//!
//! # The transport, from the real header
//!
//! `flutter_messenger.h` (shipped in the engine artifact cache) declares
//! exactly what this module uses:
//!
//! ```text
//! typedef struct { size_t struct_size; const char* channel;
//!                  const uint8_t* message; size_t message_size;
//!                  const FlutterDesktopMessageResponseHandle* response_handle;
//!                } FlutterDesktopMessage;
//! typedef void (*FlutterDesktopMessageCallback)(FlutterDesktopMessengerRef,
//!                                               const FlutterDesktopMessage*, void*);
//! void FlutterDesktopMessengerSetCallback(messenger, channel, callback, user_data);
//! void FlutterDesktopMessengerSendResponse(messenger, handle, data, data_length);
//! bool FlutterDesktopMessengerSend(messenger, channel, message, message_size);
//! ```
//!
//! Where macOS needed `block2` to construct Objective-C blocks, this is a C
//! function pointer with a `user_data` slot. The *keepalive concern*
//! transfers even though the mechanism does not: the engine stores
//! `user_data` past registration, so the state it points at must live until
//! the callback is unregistered — see [`FlutterSurface`]'s `Drop`.
//!
//! # What the spike verified about the handler
//!
//! `spikes/spike-b-windows` (FINDINGS.md W-F5) drove 25,954 Dart-initiated
//! messages through this exact registration/reply pair on this machine:
//!
//! - The message bytes are **borrowed** — valid only for the duration of the
//!   callback (the header documents the raw message data with no lifetime
//!   extension, and `struct_size` versioning implies engine-owned storage).
//!   [`inbound_text`] copies them immediately; no slice escapes the handler.
//! - `message` may be **null** for an empty payload, which is why
//!   [`inbound_text`] has a null arm rather than treating null as a bug.
//! - The handler is invoked **inline on the platform thread**; a reply sent
//!   from inside it is already on the right thread.
//! - `response_handle` is **single-use** ("Once this has been called, |handle|
//!   is invalid and must not be used again") and may be null when no reply is
//!   expected — a null handle serves the request for its side effects only.
//!
//! # Why one fixed channel name is safe
//!
//! Callback registrations are per-engine (the messenger belongs to one
//! engine), and this backend keeps one engine per
//! [`duet_supervisor::SurfaceId`] (see [`crate::backend::WinBackend`]). Two
//! Flutter surfaces therefore register [`DUET_RPC_CHANNEL`] against two
//! *different* messengers, and traffic cannot cross-wire between them.
//!
//! # Panics must not cross the C boundary
//!
//! The registered callback is `unsafe extern "C"`; a Rust panic unwinding out
//! of it into the engine's C++ frames is undefined behavior (in practice an
//! abort). Every path through the handler is therefore wrapped in
//! [`std::panic::catch_unwind`] — same rule as the macOS block thunk, same
//! placement.

use std::cell::Cell;
use std::ffi::{CString, c_void};
use std::panic::{self, AssertUnwindSafe};

use duet_command::{CommandEntry, Commands};
use duet_core::{Notification, SubscriberId};
use duet_host::BackendError;
use duet_protocol::Push;
use duet_runtime::StoreHandle;

use crate::engine::{FlutterEngine, Messenger};
use crate::ffi;

/// The one channel the whole protocol travels on, in both directions.
///
/// Must match `duetChannelName` in `packages/duet/lib/src/duet_message.dart`,
/// which is what `packages/duet_flutter` builds its `duetRpcChannel` from.
/// The Dart side uses `BasicMessageChannel<String>` with `StringCodec`, which
/// puts the payload on the wire as raw UTF-8 with no envelope and no length
/// prefix — exactly the shape [`duet_protocol::handle_text`] already consumes
/// and produces, and the same primitive
/// `FlutterEngine::set_lifecycle_state` already drives for
/// `flutter/lifecycle`.
pub const DUET_RPC_CHANNEL: &str = "duet/rpc";

/// The largest guest request this host will decode, in bytes.
///
/// A guest is a separate renderer whose messages are untrusted. Without a cap
/// it could name an arbitrarily large payload and make the host allocate to
/// match — the copy out of the borrowed message bytes, then the `String`,
/// then whatever `serde_json` builds on top. The check runs against
/// `message_size` *before* anything is copied, so an oversized request costs
/// the host nothing beyond a fixed-size reply.
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

/// Everything the registered callback needs, boxed so the engine's
/// `user_data` slot has one stable pointer to it for the registration's
/// lifetime. Owned (as a raw pointer) by the [`FlutterSurface`] that
/// registered it, freed only after unregistration — see `Drop`.
struct HandlerState {
    store: StoreHandle,
    subscriber: SubscriberId,
    commands: Commands,
}

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
/// **Main thread only.** Every method here drives the engine's messenger,
/// which belongs to the platform thread; this type is `!Send` (the raw
/// pointers inside see to that), so the compiler prevents it from being moved
/// to another thread, but it cannot check that the *creating* thread was the
/// main one. That remains a caller obligation, as it is for
/// [`FlutterEngine`].
///
/// **Drop before the engine.** See this type's `Drop` for the required order.
pub struct FlutterSurface {
    /// The engine's messenger, `AddRef`'d so replies and pushes have
    /// something to send through for as long as this surface lives.
    messenger: Messenger,
    /// The boxed [`HandlerState`] registered as the callback's `user_data`.
    /// Raw because the engine holds the same pointer; reboxed and freed in
    /// `Drop` after the callback is unregistered.
    handler_state: *mut HandlerState,
    /// This surface's host-assigned subscriber. The handler state carries its
    /// own copy; this one is what [`FlutterSurface::push`] filters against.
    subscriber: SubscriberId,
}

impl FlutterSurface {
    /// Registers the [`DUET_RPC_CHANNEL`] handler against `engine`'s
    /// messenger, serving `store` as `subscriber` and running **no commands**.
    ///
    /// The entry point for a guest that shares state and nothing else; an
    /// `invoke` from it is answered with a `failed` naming the command. Use
    /// [`with_commands`](FlutterSurface::with_commands) to give a guest a
    /// registry.
    ///
    /// # Errors
    ///
    /// [`BackendError::Unavailable`] if the engine has already shut down, so
    /// the handler could not be registered.
    pub fn new(
        engine: &FlutterEngine,
        store: StoreHandle,
        subscriber: SubscriberId,
    ) -> Result<Self, BackendError> {
        FlutterSurface::with_commands(engine, store, subscriber, &[])
    }

    /// Registers the [`DUET_RPC_CHANNEL`] handler against `engine`'s
    /// messenger, serving `store` as `subscriber` and able to run `commands`.
    ///
    /// # `commands` **is** this surface's authorization boundary
    ///
    /// Exactly as it is for [`crate::WebviewSurface::with_commands`], and the
    /// point of stating it on both: two guests over one store can be given two
    /// different registries, and neither has any vocabulary for what it was not
    /// given.
    ///
    /// # Errors
    ///
    /// [`BackendError::Unavailable`] if the engine has already shut down, so
    /// the handler could not be registered.
    pub fn with_commands(
        engine: &FlutterEngine,
        store: StoreHandle,
        subscriber: SubscriberId,
        commands: &'static [CommandEntry],
    ) -> Result<Self, BackendError> {
        let messenger = engine.binary_messenger()?;

        // The state is boxed once, here, and the raw pointer becomes the
        // registration's `user_data`. `Commands::from_entries` runs once at
        // construction rather than per message — it allocates a boxed closure
        // per entry, and doing that inside the handler would put it on the
        // path of every `get` a guest makes. Everything in the box is
        // `Send + Sync` (`StoreHandle` is a channel sender, `SubscriberId` is
        // `Copy`, `Commands` is a static table), though the callback only
        // ever runs on the platform thread anyway (W-F5).
        let handler_state = Box::into_raw(Box::new(HandlerState {
            store,
            subscriber,
            commands: Commands::from_entries(commands),
        }));

        let channel = rpc_channel_name();
        // SAFETY: `messenger.raw()` is a live, AddRef'd messenger;
        // `handler_state` is the stable heap pointer just created, which this
        // surface keeps alive until after unregistration; `rpc_trampoline`
        // matches the header's callback signature. Registration must happen
        // on the platform thread — the module-level caller obligation. The
        // Lock/IsAvailable guard makes registration against an already-dead
        // engine a clean error instead of a use-after-free.
        let registered = unsafe {
            ffi::FlutterDesktopMessengerLock(messenger.raw());
            let available = ffi::FlutterDesktopMessengerIsAvailable(messenger.raw());
            if available {
                ffi::FlutterDesktopMessengerSetCallback(
                    messenger.raw(),
                    channel.as_ptr(),
                    Some(rpc_trampoline),
                    handler_state as *mut c_void,
                );
            }
            ffi::FlutterDesktopMessengerUnlock(messenger.raw());
            available
        };
        if !registered {
            // SAFETY: the box was never registered, so this is the only
            // pointer to it.
            drop(unsafe { Box::from_raw(handler_state) });
            return Err(BackendError::Unavailable(
                "cannot register the duet/rpc handler — the engine has shut down".to_string(),
            ));
        }

        Ok(FlutterSurface {
            messenger,
            handler_state,
            subscriber,
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
    /// [`BackendError::Unavailable`] if the messenger refused the send — for
    /// example, an engine that has already shut down.
    pub fn push(&self, note: &Notification) -> Result<(), BackendError> {
        if !is_addressed_to(note, self.subscriber) {
            return Ok(());
        }
        let text = duet_protocol::push_text(&Push::Notification(note.clone()));
        let channel = rpc_channel_name();
        if self.messenger.send(&channel, text.as_bytes()) {
            Ok(())
        } else {
            Err(BackendError::Unavailable(
                "duet/rpc push was refused — engine shut down?".to_string(),
            ))
        }
    }

    /// This surface's host-assigned subscriber — the one its handler serves
    /// and the one [`FlutterSurface::push`] filters against.
    pub fn subscriber(&self) -> SubscriberId {
        self.subscriber
    }
}

impl Drop for FlutterSurface {
    /// Unregisters the channel handler, then frees the handler state.
    ///
    /// **This is not optional, and the order matters.** The engine holds the
    /// `user_data` pointer past registration (the C analog of macOS's
    /// `_messengerHandlers` map, which `shutDownEngine` never cleared), so a
    /// surface dropped without unregistering would leave the engine holding a
    /// dangling pointer. Drop the `FlutterSurface` **before** the engine it
    /// was built from shuts down:
    ///
    /// ```text
    /// drop(FlutterSurface)    -> SetCallback(duet/rpc, null)   (here)
    /// FlutterEngine::detach   -> flutter/lifecycle, park the view
    /// FlutterEngine::shutdown -> FlutterDesktopViewControllerDestroy
    /// ```
    ///
    /// Freeing the state immediately after unregistration is sound because
    /// the callback only ever runs inline on the platform thread (W-F5) —
    /// the same thread this destructor runs on, so no invocation can be in
    /// flight concurrently. If the engine is already gone, the registration
    /// died with it and only the free is needed; the availability guard
    /// absorbs that case, since a destructor cannot report and there is no
    /// alternative action to take.
    fn drop(&mut self) {
        let channel = rpc_channel_name();
        // SAFETY: the messenger is AddRef'd for this surface's lifetime;
        // Lock/IsAvailable guard the unregistration against a shut-down
        // engine. After SetCallback(null) returns (or the engine is
        // unavailable, meaning the dispatcher is gone), no further callback
        // invocation can observe `handler_state`, and this destructor runs on
        // the platform thread where invocations happen — so reboxing and
        // freeing the state here cannot race one.
        unsafe {
            ffi::FlutterDesktopMessengerLock(self.messenger.raw());
            if ffi::FlutterDesktopMessengerIsAvailable(self.messenger.raw()) {
                ffi::FlutterDesktopMessengerSetCallback(
                    self.messenger.raw(),
                    channel.as_ptr(),
                    None,
                    std::ptr::null_mut(),
                );
            }
            ffi::FlutterDesktopMessengerUnlock(self.messenger.raw());
            drop(Box::from_raw(self.handler_state));
        }
    }
}

/// [`DUET_RPC_CHANNEL`] as the NUL-terminated C string the messenger API
/// takes. Built per call site — the name is 8 bytes and the alternative is a
/// `static` with `unsafe` initialization ceremony.
fn rpc_channel_name() -> CString {
    CString::new(DUET_RPC_CHANNEL).expect("channel name has no interior NUL")
}

/// The registered `FlutterDesktopMessageCallback`.
///
/// # Safety
///
/// Only ever invoked by the engine, on the platform thread, with `message`
/// pointing at a live `FlutterDesktopMessage` and `user_data` being the
/// `HandlerState` pointer registered alongside it (kept alive by the owning
/// [`FlutterSurface`] until after unregistration).
unsafe extern "C" fn rpc_trampoline(
    messenger: ffi::MessengerRef,
    message: *const ffi::FlutterDesktopMessage,
    user_data: *mut c_void,
) {
    // SAFETY: per this function's contract, both pointers are live for the
    // duration of this call.
    let state = unsafe { &*(user_data as *const HandlerState) };
    let message = unsafe { &*message };

    // Exactly one reply per invocation, tracked rather than reasoned about.
    // The response handle is single-use ("Once this has been called, |handle|
    // is invalid and must not be used again" — flutter_messenger.h), so
    // replying twice would hand the engine a spent handle — a far worse
    // outcome than the hang it would be papering over. Today no panic can
    // occur between the reply and the end of `serve`, but that is a property
    // of the current body, not of the design; this makes it a property of the
    // design. Fresh per invocation, so the handler stays stateless.
    let replied = Cell::new(false);
    // A panic here would unwind through an `extern "C"` boundary into the
    // engine's C++ frames, which is undefined behavior — so the *entire* body
    // runs under `catch_unwind`, not just the parts that look fallible.
    // `handle_text_with` is total for protocol input, but a user command body
    // reached through it is arbitrary code.
    let served = panic::catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: the message's bytes are live for this call (they are copied
        // before it returns), the response handle — when present — is unspent,
        // and we are on the platform thread, which is where `serve`'s own
        // contract requires it to run.
        unsafe { serve(state, messenger, message, &replied) };
    }));
    if served.is_err() && !replied.get() && !message.response_handle.is_null() {
        // The guest is waiting on this reply; leaving it unanswered would
        // hang that call forever. Wrapped in its own `catch_unwind` because a
        // panic escaping the *recovery* path is no more survivable than the
        // original one.
        let _ = panic::catch_unwind(AssertUnwindSafe(|| {
            // SAFETY: same contract as above — the handle is still unspent,
            // because `replied` proves nothing has consumed it yet.
            unsafe { reply_with(messenger, message.response_handle, PANIC_FAILURE, &replied) };
        }));
    }
}

/// Serves one guest message and answers it through the response handle,
/// recording in `replied` that the handle was consumed.
///
/// A null `response_handle` (a fire-and-forget send from the guest) serves
/// the request for its side effects and drops the output — there is nothing
/// to answer through.
///
/// # Safety
///
/// Caller must be on the platform thread. `message.message` must be null or
/// point at `message.message_size` bytes valid for the duration of this call,
/// and `message.response_handle`, when non-null, must be a live, unspent
/// response handle.
unsafe fn serve(
    state: &HandlerState,
    messenger: ffi::MessengerRef,
    message: &ffi::FlutterDesktopMessage,
    replied: &Cell<bool>,
) {
    // SAFETY: delegated to this function's own contract on the bytes.
    let Some(text) = (unsafe { inbound_text(message.message, message.message_size) }) else {
        if !message.response_handle.is_null() {
            // SAFETY: the handle is live and unspent per this function's
            // contract; nothing above can have consumed it.
            unsafe {
                reply_with(
                    messenger,
                    message.response_handle,
                    OVERSIZE_FAILURE,
                    replied,
                )
            };
        }
        return;
    };
    // Total by construction: `handle_text` answers malformed guest input with
    // a `failed` response rather than an error, so there is nothing to handle
    // here. `subscriber` is the host-supplied one; a `subscriber` field on
    // the wire is ignored.
    let out =
        duet_protocol::handle_text_with(&state.store, state.subscriber, &state.commands, &text);
    if !message.response_handle.is_null() {
        // SAFETY: the handle is live and unspent on this path.
        unsafe { reply_with(messenger, message.response_handle, &out, replied) };
    }
}

/// Copies the handler's message into an owned `String`, or returns `None` if
/// it exceeds [`MAX_INBOUND_BYTES`].
///
/// The cap is checked against `len` *before* the copy, so an oversized
/// message is refused without the host ever allocating for it.
///
/// # Safety
///
/// `bytes` must be null or point at `len` bytes that stay valid for the
/// duration of this call.
unsafe fn inbound_text(bytes: *const u8, len: usize) -> Option<String> {
    // A null message pointer is how the engine represents an empty payload,
    // not an error. `handle_text("")` answers it with a `failed` response.
    if bytes.is_null() {
        return Some(String::new());
    }
    if exceeds_inbound_cap(len) {
        return None;
    }
    // The engine owns the buffer and may reuse it the moment the handler
    // returns, so this copy is mandatory, not an optimization to remove
    // later: no slice of it may outlive the handler.
    // SAFETY: `bytes` is non-null and valid for `len` bytes per this
    // function's contract.
    let borrowed = unsafe { std::slice::from_raw_parts(bytes, len) };
    Some(decode_inbound(borrowed))
}

/// Answers through `handle` with `text`'s bytes, marking `replied` first.
///
/// `replied` is set **before** the send, not after: if the send itself fails,
/// the engine's response handle must be assumed spent, and retrying would be
/// the double-reply this flag exists to prevent. Losing one guest call to a
/// hang is the lesser failure.
///
/// # Safety
///
/// Caller must be on the platform thread; `messenger` must be the live
/// messenger the engine passed to the callback, and `handle` a live, unspent
/// response handle.
unsafe fn reply_with(
    messenger: ffi::MessengerRef,
    handle: ffi::ResponseHandle,
    text: &str,
    replied: &Cell<bool>,
) {
    replied.set(true);
    // SAFETY: delegated to this function's contract; the bytes are live for
    // the call and the engine copies what it needs before returning.
    unsafe {
        ffi::FlutterDesktopMessengerSendResponse(messenger, handle, text.as_ptr(), text.len());
    }
}

/// Whether a message of `len` bytes is over [`MAX_INBOUND_BYTES`].
///
/// A free function so the size-cap decision is testable without an engine —
/// everything else on this path needs a live messenger.
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
/// and [`crate::WebviewSurface::push`] is testable without an engine, and so
/// both surfaces enforce the boundary with one implementation rather than two
/// that can drift apart.
pub(crate) fn is_addressed_to(note: &Notification, subscriber: SubscriberId) -> bool {
    serves(note.subscriber, subscriber)
}

/// Whether traffic produced for `produced_for` belongs to the surface whose own
/// subscriber is `owner`.
///
/// The one comparison behind both filters. A notification names its addressee
/// and a [`DuetEvent::WebviewScript`](crate::DuetEvent) names the surface its
/// reply was produced for; they are the same question asked of two different
/// carriers, and answering it in two places is how they would come to disagree.
pub(crate) fn serves(produced_for: SubscriberId, owner: SubscriberId) -> bool {
    produced_for == owner
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
    // gives neither. Constructing one would also register a handler that
    // nothing could then drive. Tests that merely assert the struct has
    // fields are worse than no test, so there are none. The real proof is
    // the examples, driven against a booted engine and a Dart guest.
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
        // `packages/duet` hard-codes this string in `duetChannelName`, and
        // pins it in a test of its own; so does `packages/duet_flutter`.
        // Nothing links the languages at build time, so a rename on either
        // side would silently produce a guest that talks to a channel with no
        // host handler — which surfaces as a null reply, not an error. Pinned
        // here so the Rust side fails loudly too.
        assert_eq!(DUET_RPC_CHANNEL, "duet/rpc");
    }
}
