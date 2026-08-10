//! Marshals store notifications onto the UI thread.

use duet_core::{Notification, SubscriberId};
use duet_runtime::{Sink, SinkError};
use tao::event_loop::EventLoopProxy;

/// A user event carried from the core thread to the UI thread.
#[derive(Debug)]
pub enum DuetEvent {
    /// A batch of store notifications to deliver to guests.
    Notifications(Vec<Notification>),
    /// Ask the host to run a supervisor tick.
    Tick,
    /// JavaScript the host wants evaluated in **one** webview surface — an
    /// IPC reply, or a push.
    ///
    /// `wry`'s IPC handler is installed before `build()` hands back the
    /// `WebView`, so the handler cannot hold the webview it replies through.
    /// It posts this instead, and the event loop — which does own the
    /// webview — evaluates it on the next turn.
    ///
    /// # Why it names a subscriber
    ///
    /// The event loop owns every surface and this event owns none of them, so
    /// without a name on it the loop can only guess. With one webview that
    /// guess is always right; with two it is right half the time, and the
    /// failure is a reply delivered to the **wrong guest** — which settles the
    /// wrong pending call, hangs the right one, and hands one renderer another
    /// renderer's data. A `SubscriberId` is what every surface already has and
    /// what [`crate::WebviewSurface::push`] already filters notifications on,
    /// so routing a reply is the same decision made with the same value; see
    /// [`crate::WebviewSurface::deliver`].
    WebviewScript {
        /// The surface this script was produced for.
        subscriber: SubscriberId,
        /// The JavaScript to evaluate there.
        script: String,
    },
}

/// Marshals notification batches onto the UI thread via `tao`'s proxy.
///
/// `spikes/spike-b-windows` measured this mechanism at 723 events sent and
/// 723 received over 180 seconds with zero loss, driving both a Flutter
/// platform channel and a webview `evaluate_script` from a single handler —
/// the same result Spike B measured on macOS (709/709). One marshalling
/// mechanism on every platform is the design's point; no `PostMessage` path
/// is needed.
#[derive(Debug)]
pub struct ProxySink {
    proxy: EventLoopProxy<DuetEvent>,
}

impl ProxySink {
    /// Wraps an event loop proxy.
    pub fn new(proxy: EventLoopProxy<DuetEvent>) -> Self {
        ProxySink { proxy }
    }
}

impl Sink for ProxySink {
    /// Posts the batch to the UI thread and returns immediately.
    ///
    /// Does no serialization: `deliver` runs on the core thread, so anything
    /// done here is head-of-line latency for every subsequent reader.
    ///
    /// # Errors
    ///
    /// [`SinkError::Closed`] once the event loop has exited. `duet-runtime`
    /// treats that as non-fatal — a dead UI must not take the store down.
    fn deliver(&self, batch: Vec<Notification>) -> Result<(), SinkError> {
        self.proxy
            .send_event(DuetEvent::Notifications(batch))
            .map_err(|_| SinkError::Closed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use duet_core::{Patch, Path, SubscriberId, SubscriptionId, Value};
    use tao::event_loop::EventLoopBuilder;
    use tao::platform::windows::EventLoopBuilderExtWindows;

    fn note() -> Notification {
        Notification {
            subscriber: SubscriberId(1),
            subscription: SubscriptionId(1),
            patch: Patch {
                path: Path::parse("editor.zoom").expect("test path should parse"),
                value: Value::Float(1.0),
            },
        }
    }

    // The macOS sibling of this test is `#[ignore]`d: tao asserts the event
    // loop is built on the process's main thread there, and the test harness
    // runs tests on worker threads. On Windows, tao exposes
    // `EventLoopBuilderExtWindows::with_any_thread`, so the closed-loop path
    // is actually runnable under the ordinary harness — the first platform
    // backend where this contract is exercised by `cargo test` rather than
    // verified by hand.
    #[test]
    fn delivering_to_a_closed_loop_reports_closed_rather_than_panicking() {
        // Build a loop, take a proxy, drop the loop. A UI that has exited must
        // not take the core thread down with it — `duet-runtime` treats a
        // closed sink as non-fatal, and this is the shape that produces it.
        let sink = {
            let event_loop = EventLoopBuilder::<DuetEvent>::with_user_event()
                .with_any_thread(true)
                .build();
            ProxySink::new(event_loop.create_proxy())
        };
        assert_eq!(sink.deliver(vec![note()]), Err(SinkError::Closed));
    }

    #[test]
    fn proxy_sink_is_send_and_static() {
        // `Sink` requires `Send + 'static` because the core thread owns it.
        fn assert_sink<S: Sink>() {}
        assert_sink::<ProxySink>();
    }
}
