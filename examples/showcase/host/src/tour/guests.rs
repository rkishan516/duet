//! Booting, reading back, and tearing down the two guests.
//!
//! Both are constructed with `with_commands(..., &COMMANDS)`. That slice is the
//! authorization boundary — a surface can invoke exactly what it was handed, and
//! nothing else — so passing the same table to both is a decision, not a
//! default: two surfaces over one store could just as easily be given two
//! different tables.

use duet_core::SubscriberId;
use duet_host::WindowBackend;
use duet_runtime::StoreHandle;
use duet_supervisor::SurfaceId;

use duet_showcase::commands::COMMANDS;

use crate::tour::PlatformBackend;
use crate::tour::backend::FlutterSurface;

/// Boots a Flutter engine for `surface` and registers the `duet/rpc` handler on
/// it.
///
/// No window yet: `open_window` needs the event loop's window target, which
/// only exists inside the loop. The engine boots first either way — the engine
/// run is synchronous on both platforms (macOS's `runWithEntrypoint:`;
/// `FlutterDesktopEngineRun` per the Windows spike's W-F4), so by the time
/// this returns the Dart `main()` is already running and knocking on the
/// channel.
///
/// # Errors
///
/// Returns a human-readable reason if the engine will not boot or the handler
/// will not register.
pub fn boot_flutter(
    backend: &mut PlatformBackend,
    surface: SurfaceId,
    store: StoreHandle,
    subscriber: SubscriberId,
) -> Result<FlutterSurface, String> {
    backend
        .start_renderer(surface)
        .map_err(|e| format!("booting a Flutter engine failed: {e}"))?;
    let engine = backend
        .engine(surface)
        .ok_or_else(|| "the backend reported a booted renderer but has no engine".to_string())?;
    FlutterSurface::with_commands(engine, store, subscriber, &COMMANDS)
        .map_err(|e| format!("registering the duet/rpc handler failed: {e}"))
}

/// Tears the Flutter guest all the way down: handler, engine, window.
///
/// The order is load-bearing. `FlutterSurface`'s `Drop` unregisters the channel
/// handler, and destroying the engine does not clear its handler registration
/// on either platform (macOS's `shutDownEngine` keeps its handler map; the
/// Windows engine holds the handler's `user_data` pointer past registration) —
/// so the surface must go first. The window goes last, because destroying the
/// renderer does not close it.
///
/// `detach_view` is deliberately skipped. It is the documented middle step, but
/// `crates/duet-backend-macos/FINDINGS.md` F1 describes a real, reproduced
/// backing-store retry storm in a detached-view engine, and each backend's
/// `examples/hot_reload.rs` already establishes that destroying a renderer with
/// its view still attached is safe. A demo is the wrong place to walk into a
/// known hazard for the sake of symmetry.
///
/// # Errors
///
/// Returns a human-readable reason if the renderer will not be destroyed.
pub fn tear_down_flutter(
    backend: &mut PlatformBackend,
    surface: SurfaceId,
    handler: Option<FlutterSurface>,
) -> Result<(), String> {
    drop(handler);
    backend
        .destroy_renderer(surface)
        .map_err(|e| format!("destroying the Flutter renderer failed: {e}"))?;
    backend.close_window(surface);
    Ok(())
}

/// What the host can see of the webview guest from outside the store.
#[derive(Debug, Default, Clone)]
pub struct WebProbe {
    /// The `wry` bootstrap page has run and `window.__duet` exists.
    pub bootstrapped: bool,
    /// The showcase bundle has been evaluated and its panel is up.
    pub mounted: bool,
    /// Whatever the bundle recorded going wrong, or empty.
    pub trouble: String,
}

/// Reads the two facts the host needs that are not in the store.
///
/// Returns a **string**, not an object, and the parser below is deliberately
/// trivial: `evaluate_script_with_callback` is an observation channel, never the
/// protocol, and adding a JSON dependency to the host to decode a two-bit answer
/// would be paying real cost for a diagnostic.
///
/// Tolerates running before either script has executed, which is normal on the
/// first turns.
pub const PROBE_JS: &str = r#"(function () {
  var s = window.__duetShowcase;
  return (window.__duet ? "1" : "0")
    + (s && s.mounted ? "1" : "0")
    + "|" + ((s && s.trouble) ? String(s.trouble) : "");
})()"#;

/// Parses [`PROBE_JS`]'s answer.
///
/// `wry` hands back the JSON encoding of the returned value, so a returned
/// string arrives quoted. Anything unexpected becomes a default probe: a
/// malformed diagnostic must not be able to stall the tour.
pub fn parse_probe(json: &str) -> WebProbe {
    let body = json.trim().trim_matches('"');
    let mut chars = body.chars();
    let bootstrapped = chars.next() == Some('1');
    let mounted = chars.next() == Some('1');
    if chars.next() != Some('|') {
        return WebProbe::default();
    }
    WebProbe {
        bootstrapped,
        mounted,
        trouble: chars.collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_quoted_answer_parses() {
        let probe = parse_probe("\"11|\"");
        assert!(probe.bootstrapped && probe.mounted);
        assert_eq!(probe.trouble, "");
    }

    #[test]
    fn trouble_survives_the_trip() {
        let probe = parse_probe("\"10|TypeError: nope\"");
        assert!(probe.bootstrapped);
        assert!(!probe.mounted);
        assert_eq!(probe.trouble, "TypeError: nope");
    }

    #[test]
    fn anything_unexpected_is_not_ready() {
        assert!(!parse_probe("null").bootstrapped);
        assert!(!parse_probe("").mounted);
    }
}
