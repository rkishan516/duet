//! The Linux Flutter surface: a Dart guest wired to the shared store over a
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
//! `fl_binary_messenger.h` declares exactly what this module uses:
//!
//! ```text
//! typedef void (*FlBinaryMessengerMessageHandler)(
//!     FlBinaryMessenger*, const gchar* channel, GBytes* message,
//!     FlBinaryMessengerResponseHandle*, gpointer user_data);
//! void fl_binary_messenger_set_message_handler_on_channel(
//!     messenger, channel, handler, user_data, destroy_notify);
//! gboolean fl_binary_messenger_send_response(
//!     messenger, response_handle, GBytes* response, GError**);
//! void fl_binary_messenger_send_on_channel(
//!     messenger, channel, GBytes*, GCancellable*, GAsyncReadyCallback, gpointer);
//! ```
//!
//! Where macOS needed Objective-C blocks and Windows a bare function pointer,
//! this is GObject's flavor of the same seam: a C function pointer with a
//! `user_data` slot and an optional `GDestroyNotify`. This crate passes no
//! destroy notify and frees the state by hand after unregistration, exactly
//! as the Windows crate does — one ownership story across both, rather than
//! two that can drift.
//!
//! # What the spike verified about the handler
//!
//! `spikes/spike-b-linux` (FINDINGS.md) drove 10,734 Dart-initiated messages
//! through this registration/reply pair on this machine. The handler is
//! invoked on the GTK main context; the `GBytes` message may be **null** for
//! an empty payload; the response handle is **single-use**, answered through
//! `fl_binary_messenger_send_response`.
//!
//! # Why one fixed channel name is safe
//!
//! Handler registrations are per-engine (the messenger belongs to one
//! engine), and this backend keeps one engine per
//! [`duet_supervisor::SurfaceId`] (see [`crate::backend::LinuxBackend`]).
//! Two Flutter surfaces therefore register [`DUET_RPC_CHANNEL`] against two
//! *different* messengers, and traffic cannot cross-wire between them.
//!
//! # Panics must not cross the C boundary
//!
//! The registered callback is `unsafe extern "C"`; a Rust panic unwinding out
//! of it into the embedder's C++ frames is undefined behavior. Every path
//! through the handler is therefore wrapped in [`std::panic::catch_unwind`] —
//! the same rule as on the other two platforms, same placement.

use std::cell::Cell;
use std::ffi::{CString, c_char, c_void};
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
/// and produces, and the same primitive the engine's lifecycle sends drive
/// for `flutter/lifecycle`.
pub const DUET_RPC_CHANNEL: &str = "duet/rpc";

/// The largest guest request this host will decode, in bytes.
///
/// A guest is a separate renderer whose messages are untrusted. Without a cap
/// it could name an arbitrarily large payload and make the host allocate to
/// match. The check runs against `g_bytes_get_size` *before* anything is
/// copied, so an oversized request costs the host nothing beyond a
/// fixed-size reply. 1 MiB for the same reasons the other two platforms give
/// at length.
const MAX_INBOUND_BYTES: usize = 1024 * 1024;

/// The reply to a request that exceeded [`MAX_INBOUND_BYTES`].
///
/// A `const &str`, not a formatted string: this is emitted on a path that
/// exists precisely because the host is refusing to allocate for this guest.
/// The id is `"0"` because the request was never decoded — reading its real
/// id would mean parsing the very payload being refused.
const OVERSIZE_FAILURE: &str = concat!(
    r#"{"kind":"failed","id":"0","#,
    r#""message":"request exceeds the host's inbound size limit"}"#
);

/// The reply sent when serving a request panicked.
///
/// A `const &str` for the stronger reason: this runs *after* a panic was
/// caught, so the recovery path must not be able to panic itself — no
/// formatting, no allocation, and the caught panic's message deliberately
/// not included.
const PANIC_FAILURE: &str = concat!(
    r#"{"kind":"failed","id":"0","#,
    r#""message":"the host failed while serving this request"}"#
);

/// Everything the registered callback needs, boxed so the messenger's
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
/// construction and never read from a message; [`duet_protocol::Request`]
/// carries no subscriber field precisely so a guest cannot subscribe as
/// another guest, and [`FlutterSurface::push`] enforces the same boundary in
/// the other direction.
///
/// **Main thread only.** Every method drives the engine's messenger on the
/// GTK main context; this type is `!Send` (the raw pointers see to that),
/// and the *creating* thread being the main one stays a caller obligation.
///
/// **Drop before `destroy_renderer`.** The handler must unregister while
/// the messenger's channel table still belongs to a live engine —
/// `destroy_renderer` forces the engine's dispose (see the shutdown notes
/// on [`crate::engine::FlutterEngine`]), after which this surface's own
/// references keep its send path memory-safe but nothing more.
pub struct FlutterSurface {
    /// The engine-referencing messenger, so replies and pushes can never
    /// dangle for as long as this surface lives.
    messenger: Messenger,
    /// The boxed [`HandlerState`] registered as the callback's `user_data`.
    /// Raw because the messenger holds the same pointer; reboxed and freed in
    /// `Drop` after the callback is unregistered.
    handler_state: *mut HandlerState,
    /// This surface's host-assigned subscriber, filtered on by
    /// [`FlutterSurface::push`].
    subscriber: SubscriberId,
}

impl FlutterSurface {
    /// Registers the [`DUET_RPC_CHANNEL`] handler against `engine`'s
    /// messenger, serving `store` as `subscriber` and running **no commands**.
    ///
    /// # Errors
    ///
    /// Infallible in practice (registration is a void GObject call against a
    /// live messenger); `Result` keeps the three backends' constructors
    /// identical.
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
    /// Exactly as on the other two platforms, and stated on all three: two
    /// guests over one store can be given two different registries, and
    /// neither has any vocabulary for what it was not given.
    ///
    /// # Errors
    ///
    /// See [`FlutterSurface::new`].
    pub fn with_commands(
        engine: &FlutterEngine,
        store: StoreHandle,
        subscriber: SubscriberId,
        commands: &'static [CommandEntry],
    ) -> Result<Self, BackendError> {
        let messenger = engine.binary_messenger()?;

        // Boxed once; the raw pointer becomes the registration's user_data.
        // `Commands::from_entries` runs here rather than per message — it
        // allocates a boxed closure per entry, which must not sit on the path
        // of every `get` a guest makes.
        let handler_state = Box::into_raw(Box::new(HandlerState {
            store,
            subscriber,
            commands: Commands::from_entries(commands),
        }));

        let channel = rpc_channel_name();
        // SAFETY: the messenger's engine is alive (the Messenger handle holds
        // a reference); `handler_state` is the stable heap pointer just
        // created, kept alive by this surface until after unregistration;
        // `rpc_trampoline` matches the header's callback type. No destroy
        // notify: the state is freed by hand in `Drop`, after unregistration,
        // the same ownership story as the Windows crate.
        unsafe {
            ffi::fl_binary_messenger_set_message_handler_on_channel(
                messenger.raw(),
                channel.as_ptr(),
                Some(rpc_trampoline),
                handler_state as *mut c_void,
                None,
            );
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
    /// subscriber is dropped and `Ok(())` returned — the confidentiality
    /// boundary written into the code rather than left to a caller's
    /// discipline, and silently, because fan-out to a non-addressee is the
    /// normal case, not a fault.
    ///
    /// # Errors
    ///
    /// Currently infallible (the send is fire-and-forget); `Result` keeps
    /// the three surfaces' shapes identical.
    pub fn push(&self, note: &Notification) -> Result<(), BackendError> {
        if !is_addressed_to(note, self.subscriber) {
            return Ok(());
        }
        let text = duet_protocol::push_text(&Push::Notification(note.clone()));
        self.messenger.send(&rpc_channel_name(), text.as_bytes());
        Ok(())
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
    /// **This is not optional, and the order matters**, exactly as on the
    /// other two platforms: the messenger holds the `user_data` pointer past
    /// registration, so a surface dropped without unregistering would leave
    /// it dangling. Drop the `FlutterSurface` **before** `destroy_renderer`,
    /// while the messenger's channel table still belongs to a live engine
    /// (see the shutdown notes on [`crate::engine::FlutterEngine`] — its
    /// dispose is forced there, and this surface's own references only keep
    /// the send path memory-safe, not Dart running).
    ///
    /// Freeing the state immediately after unregistration is sound because
    /// the handler only ever runs on the GTK main context — the same thread
    /// this destructor runs on — so no invocation can be in flight
    /// concurrently.
    fn drop(&mut self) {
        let channel = rpc_channel_name();
        // SAFETY: the messenger's engine is alive via the Messenger handle's
        // reference; passing a null handler unregisters the channel. After it
        // returns no callback can observe `handler_state`, and this thread is
        // the only one invocations ever ran on — so reboxing and freeing
        // cannot race one.
        unsafe {
            ffi::fl_binary_messenger_set_message_handler_on_channel(
                self.messenger.raw(),
                channel.as_ptr(),
                None,
                std::ptr::null_mut(),
                None,
            );
            drop(Box::from_raw(self.handler_state));
        }
    }
}

/// [`DUET_RPC_CHANNEL`] as the NUL-terminated C string the messenger API
/// takes.
fn rpc_channel_name() -> CString {
    CString::new(DUET_RPC_CHANNEL).expect("channel name has no interior NUL")
}

/// The registered `FlBinaryMessengerMessageHandler`.
///
/// # Safety
///
/// Only ever invoked by the messenger, on the GTK main context, with
/// `message` null or a live `GBytes*`, `response_handle` a live single-use
/// handle, and `user_data` the [`HandlerState`] pointer registered alongside
/// it (kept alive by the owning [`FlutterSurface`] until unregistration).
unsafe extern "C" fn rpc_trampoline(
    messenger: ffi::MessengerRef,
    _channel: *const c_char,
    message: ffi::GBytesRef,
    response_handle: ffi::ResponseHandleRef,
    user_data: *mut c_void,
) {
    // SAFETY: per this function's contract, the pointer is live for the call.
    let state = unsafe { &*(user_data as *const HandlerState) };

    // Exactly one reply per invocation, tracked rather than reasoned about:
    // the response handle is single-use, and consuming it twice would hand
    // the embedder a spent handle. Fresh per invocation, so the handler
    // stays stateless.
    let replied = Cell::new(false);
    // A panic here would unwind through an `extern "C"` boundary into the
    // embedder's C++ frames — undefined behavior — so the *entire* body runs
    // under `catch_unwind`. `handle_text_with` is total for protocol input,
    // but a user command body reached through it is arbitrary code.
    let served = panic::catch_unwind(AssertUnwindSafe(|| {
        serve(state, messenger, message, response_handle, &replied);
    }));
    if served.is_err() && !replied.get() && !response_handle.is_null() {
        // The guest is waiting on this reply; leaving it unanswered would
        // hang that call forever. Its own catch_unwind, because a panic
        // escaping the recovery path is no more survivable than the first.
        let _ = panic::catch_unwind(AssertUnwindSafe(|| {
            reply_with(messenger, response_handle, PANIC_FAILURE, &replied);
        }));
    }
}

/// Serves one guest message and answers it through the response handle,
/// recording in `replied` that the handle was consumed. A null
/// `response_handle` serves the request for its side effects only.
fn serve(
    state: &HandlerState,
    messenger: ffi::MessengerRef,
    message: ffi::GBytesRef,
    response_handle: ffi::ResponseHandleRef,
    replied: &Cell<bool>,
) {
    // The cap bites on the GBytes' length, before any copy — an oversized
    // request is refused without the host allocating for it.
    if exceeds_inbound_cap(ffi::bytes_len(message)) {
        if !response_handle.is_null() {
            reply_with(messenger, response_handle, OVERSIZE_FAILURE, replied);
        }
        return;
    }
    // A null GBytes is how the embedder represents an empty payload, not an
    // error; `handle_text("")` answers it with a `failed` response. The copy
    // out of the GBytes is mandatory — nothing may outlive the handler.
    let text = decode_inbound(&ffi::bytes_to_vec(message));
    // Total by construction: malformed guest input is answered with `failed`
    // rather than an error. `subscriber` is the host-supplied one; a wire
    // subscriber field is ignored.
    let out =
        duet_protocol::handle_text_with(&state.store, state.subscriber, &state.commands, &text);
    if !response_handle.is_null() {
        reply_with(messenger, response_handle, &out, replied);
    }
}

/// Answers through `handle` with `text`'s bytes, marking `replied` first —
/// if the send itself fails, the handle must be assumed spent, and retrying
/// would be the double-reply the flag exists to prevent.
fn reply_with(
    messenger: ffi::MessengerRef,
    handle: ffi::ResponseHandleRef,
    text: &str,
    replied: &Cell<bool>,
) {
    replied.set(true);
    let payload = ffi::bytes_new(text.as_bytes());
    let mut error: *mut glib_sys::GError = std::ptr::null_mut();
    // SAFETY: the messenger and handle are the live ones this invocation
    // carries; the payload reference is ours to drop after the call. An
    // error (spent handle, dead engine) is absorbed — there is no recovery
    // beyond not retrying, which `replied` already guarantees.
    unsafe {
        ffi::fl_binary_messenger_send_response(messenger, handle, payload, &mut error);
        if !error.is_null() {
            glib_sys::g_error_free(error);
        }
        glib_sys::g_bytes_unref(payload);
    }
}

/// Whether a message of `len` bytes is over [`MAX_INBOUND_BYTES`].
///
/// A free function so the size-cap decision is testable without an engine.
fn exceeds_inbound_cap(len: usize) -> bool {
    len > MAX_INBOUND_BYTES
}

/// Decodes guest bytes as UTF-8, replacing anything invalid.
///
/// Lossy rather than fallible on purpose: guest bytes are untrusted, and
/// invalid UTF-8 is just one more malformed request for
/// [`duet_protocol::handle_text`] to answer — not a second, transport-level
/// error shape.
fn decode_inbound(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Whether `note` is addressed to `subscriber`.
///
/// A free function so the confidentiality filter in [`FlutterSurface::push`]
/// and [`crate::WebviewSurface::push`] is testable without an engine, and so
/// both surfaces enforce the boundary with one implementation.
pub(crate) fn is_addressed_to(note: &Notification, subscriber: SubscriberId) -> bool {
    serves(note.subscriber, subscriber)
}

/// Whether traffic produced for `produced_for` belongs to the surface whose
/// own subscriber is `owner` — the one comparison behind both filters.
pub(crate) fn serves(produced_for: SubscriberId, owner: SubscriberId) -> bool {
    produced_for == owner
}

#[cfg(test)]
mod tests {
    use super::*;
    use duet_core::{Patch, Path, SubscriptionId, Value};
    use duet_protocol::{Response, decode_response};
    use duet_runtime::{NullSink, Runtime};

    // NOTE ON COVERAGE. `FlutterSurface` itself cannot be tested here: `new`,
    // `push` and `Drop` all need a live engine on a GTK main context. What
    // *is* tested is everything deliberately factored out of the handler to
    // be reachable without one — the size-cap decision, the inbound decode,
    // the subscriber filter, and the two const replies. The real proof is
    // the examples, driven against a booted engine and a Dart guest.

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
        // The invariant is *answerability*, not failure — see the identical
        // test in the other two backends for the full argument, including
        // why the third case below legitimately succeeds.
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
        let request = r#"{"kind":"get","id":"1","path":"editor.zoom","note":"café ☕"}"#;
        assert_eq!(decode_inbound(request.as_bytes()), request);
    }

    #[test]
    fn push_delivers_only_to_the_subscriber_the_note_names() {
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
        // Pinned on all three platforms and in both guest packages: nothing
        // links the languages at build time, and a silent rename surfaces as
        // a null reply, not an error.
        assert_eq!(DUET_RPC_CHANNEL, "duet/rpc");
    }
}
