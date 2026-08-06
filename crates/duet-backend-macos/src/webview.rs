//! The macOS webview surface: a `wry` WebView wired to the shared store.
//!
//! # The two guests must be hardened alike
//!
//! [`crate::FlutterSurface`] and this module sit on the *same* trust boundary:
//! both hand untrusted guest text to [`duet_protocol::handle_text`], and both
//! run their handler on the platform thread. A cap or a panic guard present on
//! only one of them is not a defence, it is a map of where to aim — and a
//! webview is the guest more likely to be running content the embedder did not
//! write.
//!
//! So this module mirrors `flutter_surface`'s two protections, for the same
//! reasons stated there at length:
//!
//! - a 1 MiB inbound cap ([`exceeds_inbound_cap`]), refusing an oversized
//!   request before the host allocates anything on its behalf, and
//! - [`std::panic::catch_unwind`] around the whole handler body, answering with
//!   a `const &str` so the recovery path neither formats nor allocates
//!   anything derived from the panic it just caught.

use std::panic::{self, AssertUnwindSafe};

use duet_core::{Notification, SubscriberId};
use duet_host::BackendError;
use duet_protocol::Push;
use duet_runtime::StoreHandle;
use tao::event_loop::EventLoopProxy;
use tao::window::Window;
use wry::{WebView, WebViewBuilder};

use crate::sink::DuetEvent;

/// The largest guest request this surface will decode, in bytes.
///
/// The same number, for the same reason, as `flutter_surface`'s cap of the same
/// name — see that module for the full argument. In short: a guest is a
/// separate renderer whose messages are untrusted, and without a cap it can
/// name an arbitrarily large payload and make the host allocate to match. The
/// check runs against the body's length *before* anything downstream copies or
/// parses it.
///
/// 1 MiB is far above any real request. `duet-protocol`'s own hostile-input
/// tests treat 1 MB payloads as the pathological case, and command RPC only
/// widens the surface: a `command` name and its `args` are both guest-chosen
/// strings.
const MAX_INBOUND_BYTES: usize = 1024 * 1024;

/// The reply to a request that exceeded [`MAX_INBOUND_BYTES`].
///
/// A `const &str`, not a formatted string: this is emitted on a path that
/// exists precisely because the host is refusing to allocate for this guest.
///
/// The id is `"0"` — [`duet_protocol::RequestId::UNCORRELATED`] — because the
/// request was never decoded, so its real id is unknown. Reading it would mean
/// parsing the very payload being refused. The guest's `WryTransport` treats an
/// uncorrelated `failed` as a refusal it cannot route and fails every
/// outstanding call rather than hanging on one of them; see
/// `#handleResponse` in `packages/duet-js/src/wry.ts`.
const OVERSIZE_FAILURE: &str = concat!(
    r#"{"kind":"failed","id":"0","#,
    r#""message":"request exceeds the host's inbound size limit"}"#
);

/// The reply sent when serving a request panicked.
///
/// A `const &str` for the same reason as [`OVERSIZE_FAILURE`], and a stronger
/// one: this runs *after* a panic was caught, so the recovery path must not be
/// able to panic itself. No formatting, no allocation derived from the failure,
/// and the caught panic's own message is deliberately not included — building
/// that string is exactly the kind of work that could fail again here, and a
/// guest can do nothing useful with it anyway.
const PANIC_FAILURE: &str = concat!(
    r#"{"kind":"failed","id":"0","#,
    r#""message":"the host failed while serving this request"}"#
);

/// A `wry` webview that speaks `duet-protocol` to the shared store.
///
/// Its IPC handler holds the surface's own [`SubscriberId`], captured at
/// construction and never read from a message. `duet_protocol::Request`
/// carries no subscriber field precisely so that a guest cannot subscribe as
/// another guest — a webview must not be able to receive the Flutter
/// surface's notifications.
pub struct WebviewSurface {
    webview: WebView,
    /// This surface's own subscriber, kept so [`WebviewSurface::push`] can
    /// refuse to deliver another guest's notification.
    subscriber: SubscriberId,
}

impl WebviewSurface {
    /// Builds a webview in `window`, wired to `store` as `subscriber`.
    ///
    /// Replies are posted to `proxy` as [`DuetEvent::WebviewScript`]; the
    /// caller's event loop must hand each one to [`WebviewSurface::eval`].
    /// Without that arm, every reply is dropped and the guest hangs.
    ///
    /// # Errors
    ///
    /// [`BackendError::Unavailable`] if `wry` could not create the webview.
    pub fn new(
        window: &Window,
        store: StoreHandle,
        subscriber: SubscriberId,
        proxy: EventLoopProxy<DuetEvent>,
    ) -> Result<Self, BackendError> {
        // The handler owns clones of everything it needs and borrows nothing
        // from this call — required, since it outlives `new`. Note wry's
        // bound is `Fn`, not `FnMut`, so it may only take `&self` on what it
        // captures; `EventLoopProxy::send_event` and `StoreHandle`'s methods
        // both satisfy that.
        let handler_store = store.clone();
        let webview = WebViewBuilder::new()
            .with_html(duet_webview::bootstrap::BOOTSTRAP_HTML)
            .with_ipc_handler(move |request| {
                // The *entire* body runs under `catch_unwind`, not just the
                // parts that look fallible — the same rule `flutter_surface`
                // states for its block thunk. `wry`'s macOS IPC handler is
                // invoked from a `WKScriptMessageHandler` callback, so a panic
                // unwinding out of here crosses into Objective-C frames, and
                // `objc2`'s mandatory `catch-all` feature (enabled
                // workspace-wide) turns any Objective-C exception raised on the
                // way into a Rust panic in the first place. `handle_text` is
                // total by construction today; a user command body reached
                // through `handle_text_with` is not, and cannot be made so.
                let served = panic::catch_unwind(AssertUnwindSafe(|| {
                    serve(&handler_store, subscriber, request.body())
                }));
                let reply = match &served {
                    Ok(reply) => reply.as_str(),
                    Err(_) => PANIC_FAILURE,
                };
                // Replies are *pushed* into the guest, never returned from an
                // evaluated script: wry runs a script's return value through
                // NSJSONSerialization, which would double-encode the JSON.
                // Spike B hit exactly that bug.
                //
                // A send failure means the event loop has already exited, so
                // there is no guest left to answer — dropping is correct.
                let _ = proxy.send_event(DuetEvent::WebviewScript(duet_webview::response_script(
                    reply,
                )));
            })
            .build(window)
            .map_err(|e| BackendError::Unavailable(format!("webview: {e}")))?;

        Ok(WebviewSurface {
            webview,
            subscriber,
        })
    }

    /// Delivers a notification to this guest, or drops it silently if it is
    /// addressed to a different one.
    ///
    /// Takes a [`Notification`] rather than a [`Push`] so the confidentiality
    /// filter cannot be bypassed: a caller holding a mixed notification stream
    /// for several surfaces must not be able to leak one guest's state into
    /// another by forgetting a check. [`crate::FlutterSurface::push`] enforces
    /// the same rule through the same predicate.
    ///
    /// # Errors
    ///
    /// [`BackendError::Unavailable`] if the script could not be evaluated.
    pub fn push(&self, note: &Notification) -> Result<(), BackendError> {
        if !crate::flutter_surface::is_addressed_to(note, self.subscriber) {
            return Ok(());
        }
        self.eval(&duet_webview::push_script(&Push::Notification(
            note.clone(),
        )))
    }

    /// Evaluates JavaScript in the guest.
    ///
    /// The event loop calls this with the payload of every
    /// [`DuetEvent::WebviewScript`] it receives.
    ///
    /// # Errors
    ///
    /// [`BackendError::Unavailable`] if the script could not be evaluated.
    pub fn eval(&self, script: &str) -> Result<(), BackendError> {
        self.webview
            .evaluate_script(script)
            .map_err(|e| BackendError::Unavailable(format!("eval: {e}")))
    }

    /// Evaluates JavaScript and hands `script`'s return value to `callback`
    /// as JSON text.
    ///
    /// This is **observation only** — the protocol never travels this way.
    /// `wry` runs a script's return value through `NSJSONSerialization`, so a
    /// script that returns an already-stringified JSON string comes back
    /// double-encoded (Spike B hit exactly that; see
    /// `spikes/spike-b-macos/src/main.rs:688`). Always return a plain
    /// JavaScript object. Real replies and pushes go through [`eval`] with the
    /// payload embedded in the script, which has no such step.
    ///
    /// `callback` runs later, on the main thread, from the webview's
    /// completion handler — not before this call returns. `wry` requires it to
    /// be `Send + 'static`, so it cannot borrow from the caller.
    ///
    /// [`eval`]: WebviewSurface::eval
    ///
    /// # Errors
    ///
    /// [`BackendError::Unavailable`] if the script could not be evaluated.
    pub fn eval_with_callback(
        &self,
        script: &str,
        callback: impl Fn(String) + Send + 'static,
    ) -> Result<(), BackendError> {
        self.webview
            .evaluate_script_with_callback(script, callback)
            .map_err(|e| BackendError::Unavailable(format!("eval with callback: {e}")))
    }
}

/// Serves one guest message and returns the JSON text to answer it with.
///
/// A free function so the whole decision — cap, then route — is reachable
/// without a window. Everything else on this path needs a live `WebView`, which
/// needs a window server, which is why `duet-backend-macos` is excluded from CI
/// entirely; a size cap that only a human on a Mac can exercise is a size cap
/// nobody checks.
///
/// Total: [`duet_protocol::handle_text`] answers malformed guest input with a
/// `failed` response rather than an error, and the oversize branch answers with
/// a `failed` of its own.
fn serve(store: &StoreHandle, subscriber: SubscriberId, body: &str) -> String {
    if exceeds_inbound_cap(body.len()) {
        return OVERSIZE_FAILURE.to_string();
    }
    duet_protocol::handle_text(store, subscriber, body)
}

/// Whether a request body of `len` bytes is over [`MAX_INBOUND_BYTES`].
///
/// Split out from [`serve`] for the same reason `flutter_surface` splits its
/// own: the boundary is where an off-by-one hides, and a boundary buried in a
/// closure that needs a window is a boundary no test can reach.
fn exceeds_inbound_cap(len: usize) -> bool {
    len > MAX_INBOUND_BYTES
}

#[cfg(test)]
mod tests {
    use super::*;
    use duet_core::Value;
    use duet_protocol::{Response, decode_response};
    use duet_runtime::{NullSink, Runtime};

    // NOTE ON COVERAGE. `WebviewSurface` itself is not tested here and cannot
    // be: `new`, `push` and `eval` all need a live `wry` `WebView`, which needs
    // a window server the default `cargo test` harness does not provide. What
    // *is* tested is everything deliberately factored out of the IPC closure so
    // it is reachable without one — the size-cap boundary, the routing around
    // it, and the two const replies. The rest is proved by the four macOS
    // examples, driven against a real webview.

    fn rt() -> Runtime {
        Runtime::spawn(Value::map([("editor", Value::Int(0))]), NullSink)
    }

    #[test]
    fn the_size_cap_admits_exactly_the_limit_and_refuses_one_byte_more() {
        // Off-by-one here is the difference between a working 1 MiB request and
        // a guest that can never send one, so the boundary is asserted rather
        // than assumed. Mirrors the identical test in `flutter_surface`: the
        // two transports must agree on the number, or the same request succeeds
        // through one guest and fails through the other.
        assert!(!exceeds_inbound_cap(0), "an empty body must be admitted");
        assert!(
            !exceeds_inbound_cap(MAX_INBOUND_BYTES),
            "a body of exactly the cap must be admitted"
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
    fn an_oversize_body_is_refused_without_being_parsed() {
        // The cap must bite on the *body length*, before the router sees it —
        // otherwise a guest still gets the host to build a `serde_json` tree
        // from a megabyte of its choosing, which is the cost being refused.
        // Built from a request that would otherwise SUCCEED, so the refusal
        // cannot be mistaken for ordinary malformed-input handling.
        let rt = rt();
        let padded = format!(
            r#"{{"kind":"get","id":"1","path":"editor","pad":"{}"}}"#,
            "p".repeat(MAX_INBOUND_BYTES)
        );
        let reply = serve(&rt.handle(), SubscriberId(1), &padded);
        assert_eq!(reply, OVERSIZE_FAILURE, "got {reply}");

        // The positive control: the same request unpadded is served normally,
        // so the test above cannot pass against a handler that refuses
        // everything.
        let ok = serve(
            &rt.handle(),
            SubscriberId(1),
            r#"{"kind":"get","id":"1","path":"editor"}"#,
        );
        let json: serde_json::Value = serde_json::from_str(&ok).expect("valid JSON");
        assert_eq!(json["kind"], "value", "got {ok}");
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn the_reply_never_scales_with_an_oversize_request() {
        // The amplification property, stated directly: a guest must not be able
        // to convert N bytes of garbage into N bytes of host-produced text.
        let rt = rt();
        let reply = serve(
            &rt.handle(),
            SubscriberId(1),
            &"z".repeat(4 * MAX_INBOUND_BYTES),
        );
        assert!(
            reply.len() < 256,
            "a 4 MiB request produced a {}-byte reply",
            reply.len()
        );
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn the_const_failure_replies_are_responses_a_guest_can_actually_decode() {
        // These two strings are hand-written rather than encoded, precisely so
        // the paths that emit them cannot allocate or panic. That trade means
        // nothing checks their shape at runtime — a typo would reach a guest as
        // an undecodable reply on exactly the paths where the guest is already
        // in trouble. So they are decoded here with the same function a guest
        // uses.
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
                        id,
                        duet_protocol::RequestId::UNCORRELATED,
                        "the {what} reply answers a request whose id was never read"
                    );
                    assert!(!message.is_empty(), "the {what} reply must say something");
                }
                other => panic!("the {what} reply must be a failure, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_panicking_handler_body_answers_the_guest_instead_of_unwinding() {
        // The guard the IPC closure wraps `serve` in, exercised against a body
        // that really panics. It cannot be driven through `serve` itself —
        // `handle_text` is total — so the closure's own recovery shape is
        // reproduced here: catch, then answer with the const.
        //
        // What this pins is that a caught panic still produces a decodable
        // reply. Without the guard the panic unwinds into Objective-C frames
        // and takes the process down, and the guest's call never settles.
        let served = panic::catch_unwind(AssertUnwindSafe(|| -> String {
            panic!("intentional panic: standing in for a user command body");
        }));
        let reply = match &served {
            Ok(reply) => reply.as_str(),
            Err(_) => PANIC_FAILURE,
        };
        assert!(served.is_err(), "the panic must have been caught");
        let json: serde_json::Value = serde_json::from_str(reply).expect("valid JSON");
        assert_eq!(json["kind"], "failed", "got {reply}");
    }
}
