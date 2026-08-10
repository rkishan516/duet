//! The Linux webview surface: a `wry` WebView (WebKitGTK) wired to the
//! shared store.
//!
//! # The two guests must be hardened alike
//!
//! [`crate::FlutterSurface`] and this module sit on the *same* trust boundary:
//! both hand untrusted guest text to [`duet_protocol::handle_text`], and both
//! run their handler on the main thread. A cap or a panic guard present on
//! only one of them is not a defence, it is a map of where to aim — and a
//! webview is the guest more likely to be running content the embedder did
//! not write. So this module mirrors `flutter_surface`'s two protections: the
//! 1 MiB inbound cap and the whole-body [`std::panic::catch_unwind`] with a
//! `const &str` recovery reply.
//!
//! This module is nearly identical to its macOS and Windows siblings — `wry`
//! is the cross-platform seam doing that work. What differs underneath: IPC
//! arrives via a WebKitGTK script-message handler, and the webview must be
//! built with `build_gtk` against the window's **vbox** — spike finding
//! L-F7: built against the window itself, the webview runs and answers every
//! eval while never actually being visible, because tao's
//! `GtkApplicationWindow` already holds its one permitted child.

use std::panic::{self, AssertUnwindSafe};

use duet_command::{CommandEntry, Commands};
use duet_core::{Notification, SubscriberId};
use duet_host::BackendError;
use duet_protocol::Push;
use duet_runtime::StoreHandle;
use tao::event_loop::EventLoopProxy;
use tao::platform::unix::WindowExtUnix;
use tao::window::Window;
use wry::{WebView, WebViewBuilder, WebViewBuilderExtUnix};

use crate::sink::DuetEvent;

/// The largest guest request this surface will decode, in bytes.
///
/// The same number, for the same reason, as `flutter_surface`'s cap of the
/// same name — see that module and its siblings for the full argument.
const MAX_INBOUND_BYTES: usize = 1024 * 1024;

/// The reply to a request that exceeded [`MAX_INBOUND_BYTES`].
///
/// A `const &str`, not a formatted string; the id is `"0"` —
/// [`duet_protocol::RequestId::UNCORRELATED`] — because the request was never
/// decoded. The guest's `WryTransport` treats an uncorrelated `failed` as a
/// refusal it cannot route and fails every outstanding call rather than
/// hanging; see `#handleResponse` in `packages/duet-js/src/wry.ts`.
const OVERSIZE_FAILURE: &str = concat!(
    r#"{"kind":"failed","id":"0","#,
    r#""message":"request exceeds the host's inbound size limit"}"#
);

/// The reply sent when serving a request panicked. A `const &str` because the
/// recovery path must not be able to panic itself.
const PANIC_FAILURE: &str = concat!(
    r#"{"kind":"failed","id":"0","#,
    r#""message":"the host failed while serving this request"}"#
);

/// A `wry` webview that speaks `duet-protocol` to the shared store.
///
/// Its IPC handler holds the surface's own [`SubscriberId`], captured at
/// construction and never read from a message; a webview must not be able to
/// receive the Flutter surface's notifications.
pub struct WebviewSurface {
    webview: WebView,
    /// This surface's own subscriber, kept so [`WebviewSurface::push`] can
    /// refuse to deliver another guest's notification.
    subscriber: SubscriberId,
}

impl WebviewSurface {
    /// Builds a webview in `window` that reads and writes `store` but can run
    /// **no commands**.
    ///
    /// # Errors
    ///
    /// [`BackendError::Unavailable`] if `wry` could not create the webview,
    /// or if the tao window carries no default vbox to build into.
    pub fn new(
        window: &Window,
        store: StoreHandle,
        subscriber: SubscriberId,
        proxy: EventLoopProxy<DuetEvent>,
    ) -> Result<Self, BackendError> {
        WebviewSurface::with_commands(window, store, subscriber, proxy, &[])
    }

    /// Builds a webview in `window`, wired to `store` as `subscriber`, able
    /// to run `commands`.
    ///
    /// Replies are posted to `proxy` as [`DuetEvent::WebviewScript`]; the
    /// caller's event loop must hand each one to [`WebviewSurface::deliver`].
    /// Without that arm, every reply is dropped and the guest hangs.
    ///
    /// # `commands` **is** this surface's authorization boundary
    ///
    /// Exactly as on the other two platforms — see
    /// [`duet_command::Commands`].
    ///
    /// # Errors
    ///
    /// [`BackendError::Unavailable`] if `wry` could not create the webview,
    /// or if the tao window carries no default vbox.
    pub fn with_commands(
        window: &Window,
        store: StoreHandle,
        subscriber: SubscriberId,
        proxy: EventLoopProxy<DuetEvent>,
        commands: &'static [CommandEntry],
    ) -> Result<Self, BackendError> {
        let handler_store = store.clone();
        // Built once, here, rather than per message — the same argument as
        // everywhere else this pattern appears.
        let handler_commands = Commands::from_entries(commands);
        // The vbox, not the window: L-F7. Built against the window itself the
        // webview runs invisibly — tao's GtkApplicationWindow already holds
        // its one permitted GtkBin child.
        let vbox = window.default_vbox().ok_or_else(|| {
            BackendError::Unavailable("the tao window carries no default vbox".to_string())
        })?;
        let webview = WebViewBuilder::new()
            .with_html(duet_webview::bootstrap::BOOTSTRAP_HTML)
            .with_ipc_handler(move |request| {
                // The *entire* body runs under `catch_unwind`: wry's Linux
                // IPC handler is invoked from a WebKitGTK script-message
                // callback, and a panic unwinding into GObject frames is
                // undefined behavior.
                let served = panic::catch_unwind(AssertUnwindSafe(|| {
                    serve(
                        &handler_store,
                        subscriber,
                        &handler_commands,
                        request.body(),
                    )
                }));
                let reply = match &served {
                    Ok(reply) => reply.as_str(),
                    Err(_) => PANIC_FAILURE,
                };
                // Replies are *pushed* into the guest, never returned from an
                // evaluated script — the double-encoding rule all three
                // platforms share. A send failure means the event loop has
                // exited and there is no guest left to answer.
                let _ = proxy.send_event(DuetEvent::WebviewScript {
                    subscriber,
                    script: duet_webview::response_script(reply),
                });
            })
            .build_gtk(vbox)
            .map_err(|e| BackendError::Unavailable(format!("webview: {e}")))?;

        Ok(WebviewSurface {
            webview,
            subscriber,
        })
    }

    /// This surface's host-assigned subscriber — the one its handler serves,
    /// the one [`WebviewSurface::push`] filters notifications on, and the
    /// one [`DuetEvent::WebviewScript`] names.
    pub fn subscriber(&self) -> SubscriberId {
        self.subscriber
    }

    /// Evaluates a [`DuetEvent::WebviewScript`] in this guest, or drops it if
    /// it was produced for a different one — the reply path's half of the
    /// confidentiality boundary, enforced with the same predicate as
    /// [`WebviewSurface::push`].
    ///
    /// # Errors
    ///
    /// [`BackendError::Unavailable`] if the script could not be evaluated.
    pub fn deliver(&self, subscriber: SubscriberId, script: &str) -> Result<(), BackendError> {
        if !crate::flutter_surface::serves(subscriber, self.subscriber) {
            return Ok(());
        }
        self.eval(script)
    }

    /// Delivers a notification to this guest, or drops it silently if it is
    /// addressed to a different one.
    ///
    /// Takes a [`Notification`] rather than a [`Push`] so the filter cannot
    /// be bypassed — the same rule as everywhere else, through the same
    /// predicate.
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
    /// **Observation only** — the protocol never travels this way, and the
    /// double-encoding rule holds verbatim on WebKitGTK: always return a
    /// plain JavaScript object, never pre-stringified JSON.
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
/// without a window; see the identical arrangement in the other two
/// backends.
fn serve(store: &StoreHandle, subscriber: SubscriberId, commands: &Commands, body: &str) -> String {
    if exceeds_inbound_cap(body.len()) {
        return OVERSIZE_FAILURE.to_string();
    }
    duet_protocol::handle_text_with(store, subscriber, commands, body)
}

/// Whether a request body of `len` bytes is over [`MAX_INBOUND_BYTES`].
fn exceeds_inbound_cap(len: usize) -> bool {
    len > MAX_INBOUND_BYTES
}

#[cfg(test)]
mod tests {
    use super::*;
    use duet_core::Value;
    use duet_protocol::{Response, decode_response};
    use duet_runtime::{NullSink, Runtime};

    // NOTE ON COVERAGE. `WebviewSurface` itself cannot be tested here — it
    // needs a live WebKitGTK webview inside a GTK window. What *is* tested is
    // everything factored out of the IPC closure: the size-cap boundary, the
    // routing around it, and the two const replies.

    fn rt() -> Runtime {
        Runtime::spawn(Value::map([("editor", Value::Int(0))]), NullSink)
    }

    #[test]
    fn a_reply_is_routed_to_the_surface_it_was_produced_for_and_no_other() {
        assert!(
            crate::flutter_surface::serves(SubscriberId(7), SubscriberId(7)),
            "a script produced for this surface must be evaluated"
        );
        assert!(
            !crate::flutter_surface::serves(SubscriberId(8), SubscriberId(7)),
            "another guest's reply must not be evaluated here"
        );
        assert!(
            !crate::flutter_surface::serves(SubscriberId(0), SubscriberId(7)),
            "subscriber 0 must not be treated as a wildcard"
        );
    }

    #[test]
    fn a_surface_with_no_commands_refuses_an_invoke_by_name() {
        let rt = rt();
        let reply = serve(
            &rt.handle(),
            SubscriberId(1),
            &Commands::from_entries(&[]),
            r#"{"kind":"invoke","id":"1","command":"subtract","args":{"t":"m","v":{}}}"#,
        );
        let json: serde_json::Value = serde_json::from_str(&reply).expect("valid JSON");
        assert_eq!(json["kind"], "failed", "got {reply}");
        assert!(
            json["message"]
                .as_str()
                .unwrap_or_default()
                .contains("subtract"),
            "got {reply}"
        );
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn the_size_cap_admits_exactly_the_limit_and_refuses_one_byte_more() {
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
        let rt = rt();
        let padded = format!(
            r#"{{"kind":"get","id":"1","path":"editor","pad":"{}"}}"#,
            "p".repeat(MAX_INBOUND_BYTES)
        );
        let reply = serve(
            &rt.handle(),
            SubscriberId(1),
            &Commands::from_entries(&[]),
            &padded,
        );
        assert_eq!(reply, OVERSIZE_FAILURE, "got {reply}");

        let ok = serve(
            &rt.handle(),
            SubscriberId(1),
            &Commands::from_entries(&[]),
            r#"{"kind":"get","id":"1","path":"editor"}"#,
        );
        let json: serde_json::Value = serde_json::from_str(&ok).expect("valid JSON");
        assert_eq!(json["kind"], "value", "got {ok}");
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn the_reply_never_scales_with_an_oversize_request() {
        let rt = rt();
        let reply = serve(
            &rt.handle(),
            SubscriberId(1),
            &Commands::from_entries(&[]),
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
