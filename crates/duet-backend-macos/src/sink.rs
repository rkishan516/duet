//! Marshals store notifications onto the UI thread.

use duet_core::Notification;
use duet_runtime::{Sink, SinkError};
use tao::event_loop::EventLoopProxy;

/// A user event carried from the core thread to the UI thread.
#[derive(Debug)]
pub enum DuetEvent {
    /// A batch of store notifications to deliver to guests.
    Notifications(Vec<Notification>),
    /// Ask the host to run a supervisor tick.
    Tick,
    /// JavaScript the host wants evaluated in a webview surface — an IPC
    /// reply, or a push.
    ///
    /// `wry`'s IPC handler is installed before `build()` hands back the
    /// `WebView`, so the handler cannot hold the webview it replies through.
    /// It posts this instead, and the event loop — which does own the
    /// webview — evaluates it on the next turn.
    WebviewScript(String),
}

/// Marshals notification batches onto the UI thread via `tao`'s proxy.
///
/// Spike B measured this mechanism at 709 events sent and 709 received over
/// 180 seconds with zero loss, driving both a Flutter platform channel and a
/// webview `evaluate_script` from a single handler.
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

    // NOTE: building a `tao` event loop off the main thread panics on macOS
    // (`tao` asserts the calling thread is the process's main thread). The
    // default `cargo test` harness runs each test on a worker thread, not
    // the main thread, so this test cannot run there. Verified locally by
    // running it with `--ignored`: it panics at
    // `tao-0.36.0/src/platform_impl/macos/event_loop.rs:167` with "On
    // macOS, `EventLoop` must be created on the main thread!". Marked
    // `#[ignore]` rather than deleted or weakened, per the plan's honesty
    // requirement — this is a real constraint, not a gap in coverage we
    // chose not to fill.
    #[test]
    #[ignore = "tao's EventLoop must be built on the main thread, which the test harness does not provide"]
    fn delivering_to_a_closed_loop_reports_closed_rather_than_panicking() {
        // Build a loop, take a proxy, drop the loop. A UI that has exited must
        // not take the core thread down with it — `duet-runtime` treats a
        // closed sink as non-fatal, and this is the shape that produces it.
        let sink = {
            let event_loop = EventLoopBuilder::<DuetEvent>::with_user_event().build();
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
