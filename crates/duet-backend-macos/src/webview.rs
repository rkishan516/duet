//! The macOS webview surface: a `wry` WebView wired to the shared store.

use duet_core::SubscriberId;
use duet_host::BackendError;
use duet_protocol::Push;
use duet_runtime::StoreHandle;
use tao::event_loop::EventLoopProxy;
use tao::window::Window;
use wry::{WebView, WebViewBuilder};

use crate::sink::DuetEvent;

/// A `wry` webview that speaks `duet-protocol` to the shared store.
///
/// Its IPC handler holds the surface's own [`SubscriberId`], captured at
/// construction and never read from a message. `duet_protocol::Request`
/// carries no subscriber field precisely so that a guest cannot subscribe as
/// another guest — a webview must not be able to receive the Flutter
/// surface's notifications.
pub struct WebviewSurface {
    webview: WebView,
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
                let reply =
                    duet_webview::handle_ipc_text(&handler_store, subscriber, request.body());
                // Replies are *pushed* into the guest, never returned from an
                // evaluated script: wry runs a script's return value through
                // NSJSONSerialization, which would double-encode the JSON.
                // Spike B hit exactly that bug.
                //
                // A send failure means the event loop has already exited, so
                // there is no guest left to answer — dropping is correct.
                let _ = proxy.send_event(DuetEvent::WebviewScript(duet_webview::response_script(
                    &reply,
                )));
            })
            .build(window)
            .map_err(|e| BackendError::Unavailable(format!("webview: {e}")))?;

        Ok(WebviewSurface { webview })
    }

    /// Delivers a push to the guest.
    ///
    /// # Errors
    ///
    /// [`BackendError::Unavailable`] if the script could not be evaluated.
    pub fn push(&self, push: &Push) -> Result<(), BackendError> {
        self.eval(&duet_webview::push_script(push))
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
}
