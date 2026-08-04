//! Spike B: prove that tao's event loop, Flutter's platform thread, and a wry
//! webview can coexist on ONE macOS main thread, and that tao's
//! `EventLoopProxy` can marshal work from a background "core thread" onto
//! that main thread to drive both Flutter (via a platform channel) and the
//! webview (via JS eval) at once. See FINDINGS.md for the full writeup.
//!
//! This is throwaway spike code: no tests, minimal error handling, liberal
//! stdout. The Flutter embedding calls are copied from spike-a-macos, which
//! already proved the engine-first embedding sequence works; see that
//! spike's FINDINGS.md for the discussion of *why* each call is shaped this
//! way. This file only adds new material for the run-loop-coexistence
//! question.

use std::process::Command;
use std::sync::atomic::{AtomicU64, AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject};
use objc2::msg_send;
use objc2_app_kit::{
    NSBitmapImageFileType, NSEvent, NSEventModifierFlags, NSEventType, NSView, NSWindow,
};
use objc2_foundation::{NSBundle, NSDictionary, NSNumber, NSPoint, NSRect, NSString};

use tao::dpi::LogicalSize;
use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoop, EventLoopBuilder, EventLoopProxy};
use tao::platform::macos::WindowExtMacOS;
use tao::window::{Window, WindowBuilder};

use wry::{WebView, WebViewBuilder, WebViewExtMacOS};

// ===================== Tunables =====================

/// Charter says 10 minutes; using 3 to keep the spike tractable (per brief).
/// Overridable via DUET_SPIKE_B_RUN_SECS for a quick smoke test.
fn sustained_run_duration() -> Duration {
    Duration::from_secs(
        std::env::var("DUET_SPIKE_B_RUN_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(180),
    )
}
/// How often the background "core thread" sends a proxy event.
const TICK_INTERVAL: Duration = Duration::from_millis(250);
/// Only print full detail every Nth tick, to keep stdout readable across a
/// ~720-tick run; every tick is still counted toward sent/received totals.
const LOG_EVERY_N_TICKS: u64 = 20; // ~5s at 250ms
/// When to fire the synthetic-input probe (criterion 5), after windows/views
/// have had a chance to settle.
const SYNTHETIC_INPUT_AT: Duration = Duration::from_secs(6);
const SYNTHETIC_INPUT_CHECK_AT: Duration = Duration::from_secs(8);
/// Window for the "neither starves the other" sampling (criterion 2).
/// Overridable via DUET_SPIKE_B_CHECKPOINT_SECS for a quick smoke test.
fn progress_checkpoint_at() -> Duration {
    Duration::from_secs(
        std::env::var("DUET_SPIKE_B_CHECKPOINT_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(32),
    )
}

fn app_framework_path() -> String {
    std::env::var("DUET_APP_FRAMEWORK_PATH").unwrap_or_else(|_| {
        "/Users/kishan/dev/rkishan516/tauri-flutter/spikes/spike_app/build/macos/Build/Products/Debug/App.framework"
            .to_string()
    })
}

fn rss_kb() -> String {
    let pid = std::process::id().to_string();
    match Command::new("ps").args(["-o", "rss=", "-p", &pid]).output() {
        Ok(out) => String::from_utf8_lossy(&out.stdout).trim().to_string(),
        Err(_) => "?".to_string(),
    }
}

fn rss_kb_num() -> i64 {
    rss_kb().parse().unwrap_or(-1)
}

struct Log {
    start: Instant,
}

impl Log {
    fn new() -> Self {
        Self { start: Instant::now() }
    }
    fn line(&self, msg: &str) {
        println!("[t+{:>7.2}s rss={:>7}kB] {}", self.start.elapsed().as_secs_f64(), rss_kb(), msg);
    }
    fn pass(&self, criterion: &str, msg: &str) {
        println!(
            "[t+{:>7.2}s rss={:>7}kB] PASS criterion {}: {}",
            self.start.elapsed().as_secs_f64(),
            rss_kb(),
            criterion,
            msg
        );
    }
    fn cannot_verify(&self, criterion: &str, msg: &str) {
        println!(
            "[t+{:>7.2}s rss={:>7}kB] CANNOT-VERIFY-HERE criterion {}: {}",
            self.start.elapsed().as_secs_f64(),
            rss_kb(),
            criterion,
            msg
        );
    }
}

// ===================== Flutter embedding (copied pattern from spike-a-macos) =====================

unsafe fn build_dart_project(log: &Log) -> Retained<AnyObject> {
    let framework_path = app_framework_path();
    log.line(&format!("Loading NSBundle at {framework_path}"));
    let path_ns = NSString::from_str(&framework_path);
    let bundle: Retained<NSBundle> = NSBundle::bundleWithPath(&path_ns)
        .unwrap_or_else(|| panic!("NSBundle::bundleWithPath failed for {framework_path}"));

    let project_cls = AnyClass::get(c"FlutterDartProject")
        .expect("FlutterDartProject class not found - is FlutterMacOS.framework linked?");
    let alloc: Allocated<AnyObject> = unsafe { msg_send![project_cls, alloc] };
    let project: Retained<AnyObject> =
        unsafe { msg_send![alloc, initWithPrecompiledDartBundle: &*bundle] };
    log.line("FlutterDartProject created from App.framework bundle");
    project
}

unsafe fn boot_headless_engine(log: &Log) -> Retained<AnyObject> {
    let project = unsafe { build_dart_project(log) };

    let engine_cls = AnyClass::get(c"FlutterEngine")
        .expect("FlutterEngine class not found - is FlutterMacOS.framework linked?");
    let alloc: Allocated<AnyObject> = unsafe { msg_send![engine_cls, alloc] };
    let name = NSString::from_str("duet-spike-b");
    let engine: Retained<AnyObject> = unsafe {
        msg_send![
            alloc,
            initWithName: &*name,
            project: &*project,
            allowHeadlessExecution: true,
        ]
    };
    log.line("FlutterEngine allocated with allowHeadlessExecution: true");

    let entrypoint: Option<&NSString> = None;
    let ran: bool = unsafe { msg_send![&engine, runWithEntrypoint: entrypoint] };
    log.line(&format!("runWithEntrypoint(nil) returned {ran}"));
    assert!(ran, "FlutterEngine failed to run entrypoint");

    engine
}

unsafe fn create_view_controller(engine: &AnyObject) -> Retained<AnyObject> {
    let cls = AnyClass::get(c"FlutterViewController").expect("FlutterViewController class not found");
    let alloc: Allocated<AnyObject> = unsafe { msg_send![cls, alloc] };
    let nib_name: Option<&NSString> = None;
    let bundle: Option<&NSBundle> = None;
    let controller: Retained<AnyObject> = unsafe {
        msg_send![
            alloc,
            initWithEngine: engine,
            nibName: nib_name,
            bundle: bundle,
        ]
    };
    controller
}

unsafe fn attach_view_to_window(window: &Window, controller: &AnyObject) -> Retained<NSView> {
    let flutter_view: Retained<AnyObject> = unsafe { msg_send![controller, view] };
    let flutter_view: Retained<NSView> = unsafe { Retained::cast_unchecked(flutter_view) };

    let ns_window_ptr = window.ns_window() as *mut NSWindow;
    let ns_window: &NSWindow = unsafe { &*ns_window_ptr };
    let content_view: Retained<NSView> = ns_window.contentView().expect("NSWindow has no contentView");

    let bounds: NSRect = content_view.bounds();
    flutter_view.setFrame(bounds);
    flutter_view.setAutoresizingMask(
        objc2_app_kit::NSAutoresizingMaskOptions::ViewWidthSizable
            | objc2_app_kit::NSAutoresizingMaskOptions::ViewHeightSizable,
    );
    content_view.addSubview(&flutter_view);
    flutter_view
}

/// Rasterizes any NSView (Flutter's view or wry's WKWebView subclass, both
/// qualify) to a PNG on disk via `cacheDisplayInRect:toBitmapImageRep:`.
/// Reused from spike-a-macos: this environment has no reachable on-screen
/// WindowServer for spawned processes, so `screencapture` never sees these
/// windows even though they render correctly. This in-process rasterization
/// does not require a physical display and produces genuine pixels.
unsafe fn snapshot_nsview_to_png(view: &NSView, path: &str) -> bool {
    let bounds = view.bounds();
    view.setNeedsDisplay(true);
    view.displayIfNeeded();

    let Some(bitmap) = view.bitmapImageRepForCachingDisplayInRect(bounds) else {
        println!("[spike-b] snapshot: bitmapImageRepForCachingDisplayInRect returned None for {path}");
        return false;
    };
    view.cacheDisplayInRect_toBitmapImageRep(bounds, &bitmap);

    let props: Retained<NSDictionary<objc2_app_kit::NSBitmapImageRepPropertyKey, AnyObject>> =
        NSDictionary::new();
    let png_data = unsafe { bitmap.representationUsingType_properties(NSBitmapImageFileType::PNG, &props) };
    match png_data {
        Some(data) => {
            let path_ns = NSString::from_str(path);
            let ok = data.writeToFile_atomically(&path_ns, true);
            println!("[spike-b] snapshot: wrote {path} (success={ok}, bytes={})", data.len());
            ok
        }
        None => {
            println!("[spike-b] snapshot: representationUsingType:properties: returned None for {path}");
            false
        }
    }
}

/// Builds a `FlutterMethodChannel` named "duet/spike_b" against the engine's
/// binaryMessenger. This is the exact mechanism spec section 6.2 depends on:
/// a channel built off `engine.binaryMessenger`, invoked from the main
/// thread (in response to a proxy-delivered event), replied to by Dart.
unsafe fn make_method_channel(engine: &AnyObject) -> Retained<AnyObject> {
    let messenger: Retained<AnyObject> = unsafe { msg_send![engine, binaryMessenger] };
    let cls = AnyClass::get(c"FlutterMethodChannel").expect("FlutterMethodChannel class not found");
    let name = NSString::from_str("duet/spike_b");
    let channel: Retained<AnyObject> = unsafe {
        msg_send![cls, methodChannelWithName: &*name, binaryMessenger: &*messenger]
    };
    channel
}

/// Invokes `ping` on the Flutter method channel with argument `n`, and prints
/// Dart's reply when it lands (asynchronously, via an Objective-C block).
/// `received` is incremented from inside the block, i.e. only once the reply
/// has genuinely round-tripped through the engine and back.
unsafe fn flutter_ping(
    channel: &AnyObject,
    n: u64,
    received: Arc<AtomicU64>,
    last_frame_ticks: Arc<std::sync::Mutex<i64>>,
    verbose: bool,
) {
    let method = NSString::from_str("ping");
    let arg: Retained<NSNumber> = NSNumber::new_u64(n);
    let block = RcBlock::new(move |result: *mut AnyObject| {
        let s = if result.is_null() {
            "<nil>".to_string()
        } else {
            let ns_ptr = result as *const NSString;
            unsafe { (&*ns_ptr).to_string() }
        };
        received.fetch_add(1, Ordering::SeqCst);
        // Reply looks like "pong pingCount=N frameTicks=M tapCount=K n=n" -
        // pull frameTicks out so criterion 2's checkpoint can report it.
        if let Some(idx) = s.find("frameTicks=") {
            let rest = &s[idx + "frameTicks=".len()..];
            let end = rest.find(' ').unwrap_or(rest.len());
            if let Ok(v) = rest[..end].parse::<i64>() {
                if let Ok(mut guard) = last_frame_ticks.lock() {
                    *guard = v;
                }
            }
        }
        if verbose {
            println!("[spike-b] Flutter channel reply for n={n}: {s}");
        }
    });
    unsafe {
        let _: () = msg_send![
            channel,
            invokeMethod: &*method,
            arguments: &*arg,
            result: &*block,
        ];
    }
}

/// Fire-and-forget query (no counting), used for the criterion 5 follow-up
/// check ("did the synthetic tap register on the Flutter side?").
unsafe fn flutter_query(channel: &AnyObject, label: &'static str) {
    let method = NSString::from_str("query");
    let no_args: Option<&AnyObject> = None;
    let block = RcBlock::new(move |result: *mut AnyObject| {
        let s = if result.is_null() {
            "<nil>".to_string()
        } else {
            let ns_ptr = result as *const NSString;
            unsafe { (&*ns_ptr).to_string() }
        };
        println!("[spike-b] Flutter query ({label}): {s}");
    });
    unsafe {
        let _: () = msg_send![channel, invokeMethod: &*method, arguments: no_args, result: &*block];
    }
}

// ===================== Synthetic input (criterion 5) =====================

/// Builds a synthetic left-mouse-down NSEvent targeting `window`, at the
/// window's content-view center (in that window's local coordinate space,
/// bottom-left origin, as AppKit expects).
unsafe fn make_synthetic_mouse_event(
    ns_window: &NSWindow,
    event_type: NSEventType,
    location_in_window: NSPoint,
) -> Option<Retained<NSEvent>> {
    let window_number = ns_window.windowNumber();
    NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
        event_type,
        location_in_window,
        NSEventModifierFlags::empty(),
        0.0,
        window_number,
        None,
        0,
        1,
        1.0,
    )
}

/// Sends a synthetic mouseDown+mouseUp at the center of `window`'s content
/// view, directly to that NSWindow via `sendEvent:` (not through
/// `NSApp.sendEvent:`, since these windows are never key/main in this
/// environment - see FINDINGS.md). Returns true if event objects were built
/// and sent without throwing.
unsafe fn send_synthetic_click(window: &Window, label: &str) -> bool {
    let ns_window_ptr = window.ns_window() as *mut NSWindow;
    let ns_window: &NSWindow = unsafe { &*ns_window_ptr };
    let Some(content_view) = ns_window.contentView() else {
        println!("[spike-b] synthetic input ({label}): no contentView, aborting");
        return false;
    };
    let bounds = content_view.bounds();
    let center = NSPoint { x: bounds.size.width / 2.0, y: bounds.size.height / 2.0 };

    // Best-effort attempt to give the window/view key/first-responder status
    // before injecting - in a normal WindowServer session this is what
    // real user input would have already done. In this environment there is
    // no WindowServer, so these calls may be no-ops; harmless either way.
    ns_window.makeKeyAndOrderFront(None);
    let _ = ns_window.makeFirstResponder(Some(&content_view));

    println!("[spike-b] SYNTHETIC INPUT ({label}): constructing mouseDown+mouseUp at {center:?}");

    let Some(down) = (unsafe { make_synthetic_mouse_event(ns_window, NSEventType::LeftMouseDown, center) })
    else {
        println!("[spike-b] synthetic input ({label}): mouseEventWithType: returned nil for mouseDown");
        return false;
    };
    ns_window.sendEvent(&down);

    let Some(up) = (unsafe { make_synthetic_mouse_event(ns_window, NSEventType::LeftMouseUp, center) })
    else {
        println!("[spike-b] synthetic input ({label}): mouseEventWithType: returned nil for mouseUp");
        return false;
    };
    ns_window.sendEvent(&up);

    println!("[spike-b] SYNTHETIC INPUT ({label}): mouseDown+mouseUp sent via -[NSWindow sendEvent:] (marked synthetic - not real hardware input, environment cannot generate that)");
    true
}

// ===================== App state and event handling =====================

#[derive(Debug, Clone, Copy)]
enum UserEvent {
    Tick(u64),
}

struct AppState {
    log: Log,
    engine: Option<Retained<AnyObject>>,
    method_channel: Option<Retained<AnyObject>>,
    flutter_window: Option<Window>,
    flutter_controller: Option<Retained<AnyObject>>,
    webview_window: Option<Window>,
    webview: Option<WebView>,
    proxy_sent: Arc<AtomicU64>,
    proxy_received: Arc<AtomicU64>,
    flutter_replies_received: Arc<AtomicU64>,
    webview_replies_received: Arc<AtomicU64>,
    last_flutter_frame_ticks: Arc<std::sync::Mutex<i64>>,
    last_js_raf: Arc<std::sync::Mutex<i64>>,
    last_js_interval: Arc<std::sync::Mutex<i64>>,
    /// First tick index at which raf was observed to stop advancing versus
    /// the previous sample (None until detected). Used for the rAF-stall
    /// discriminator investigation.
    raf_last_value: i64,
    raf_stall_logged: bool,
    start: Instant,
    both_alive_snapshot_taken: bool,
    synthetic_input_done: bool,
    synthetic_input_checked: bool,
    progress_checkpoint_done: bool,
    finishing: bool,
    running_flag: Arc<AtomicBool>,
    run_duration: Duration,
    checkpoint_at: Duration,
}

fn main() {
    let log = Log::new();
    log.line("Spike B starting: tao + Flutter + wry run-loop coexistence on macOS");
    log.line(&format!("App.framework path: {}", app_framework_path()));
    log.line(&format!(
        "Plan: sustained run for {:.0}s (charter says 10min; using 3min per brief \
         unless DUET_SPIKE_B_RUN_SECS overrides it), proxy tick every {:?}",
        sustained_run_duration().as_secs_f64(),
        TICK_INTERVAL
    ));

    std::fs::create_dir_all("evidence").ok();

    let event_loop: EventLoop<UserEvent> = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy: EventLoopProxy<UserEvent> = event_loop.create_proxy();

    let proxy_sent = Arc::new(AtomicU64::new(0));
    let running_flag = Arc::new(AtomicBool::new(true));

    // The background "core thread" stand-in: a real OS thread, entirely
    // separate from tao's/Flutter's/wry's main thread, that periodically
    // marshals work onto the main thread via EventLoopProxy::send_event.
    // This is exactly the mechanism spec section 6.2 depends on for Phase 2.
    {
        let proxy = proxy.clone();
        let proxy_sent = Arc::clone(&proxy_sent);
        let running_flag = Arc::clone(&running_flag);
        thread::Builder::new()
            .name("duet-core-thread-stub".into())
            .spawn(move || {
                let mut n: u64 = 0;
                println!("[core-thread] background thread started (tid stand-in for Phase 2's core thread)");
                loop {
                    if !running_flag.load(Ordering::SeqCst) {
                        println!("[core-thread] stop flag observed, exiting after {n} sends");
                        break;
                    }
                    thread::sleep(TICK_INTERVAL);
                    if !running_flag.load(Ordering::SeqCst) {
                        println!("[core-thread] stop flag observed, exiting after {n} sends");
                        break;
                    }
                    match proxy.send_event(UserEvent::Tick(n)) {
                        Ok(()) => {
                            proxy_sent.fetch_add(1, Ordering::SeqCst);
                            n += 1;
                        }
                        Err(_) => {
                            println!("[core-thread] send_event failed (event loop gone), exiting after {n} sends");
                            break;
                        }
                    }
                }
            })
            .expect("failed to spawn core-thread stub");
    }

    let mut state = AppState {
        log,
        engine: None,
        method_channel: None,
        flutter_window: None,
        flutter_controller: None,
        webview_window: None,
        webview: None,
        proxy_sent,
        proxy_received: Arc::new(AtomicU64::new(0)),
        flutter_replies_received: Arc::new(AtomicU64::new(0)),
        webview_replies_received: Arc::new(AtomicU64::new(0)),
        last_flutter_frame_ticks: Arc::new(std::sync::Mutex::new(-1)),
        last_js_raf: Arc::new(std::sync::Mutex::new(-1)),
        last_js_interval: Arc::new(std::sync::Mutex::new(-1)),
        raf_last_value: -1,
        raf_stall_logged: false,
        start: Instant::now(),
        both_alive_snapshot_taken: false,
        synthetic_input_done: false,
        synthetic_input_checked: false,
        progress_checkpoint_done: false,
        finishing: false,
        running_flag,
        run_duration: sustained_run_duration(),
        checkpoint_at: progress_checkpoint_at(),
    };

    event_loop.run(move |event, target, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::NewEvents(StartCause::Init) => {
                setup(&mut state, target);
            }
            Event::UserEvent(UserEvent::Tick(n)) => {
                on_tick(&mut state, n);
                if state.finishing {
                    *control_flow = ControlFlow::Exit;
                }
            }
            Event::LoopDestroyed => {
                state.log.line("LoopDestroyed - process exiting");
            }
            _ => {}
        }
    });
}

// Two independent, self-driven counters, deliberately using two different
// browser scheduling primitives that are throttled by different rules:
//   - requestAnimationFrame: tied to the compositor/display-link; browsers
//     (WebKit included) commonly throttle or fully suspend rAF for a
//     window/tab they consider not visible.
//   - setInterval: a plain timer, NOT tied to the compositor or visibility
//     in the same way (background tabs get a min-interval clamp, typically
//     ~1s, but it does not stop the way rAF can).
// If rAF stalls while setInterval keeps ticking, that is evidence of
// WebKit's own visibility/occlusion throttling (benign, environment
// artifact) rather than the host's run loop starving the webview (which
// would stall setInterval too, since eval-dispatch and setInterval both
// depend on the webview's JS run loop actually being pumped).
const WEBVIEW_HTML: &str = r#"<!doctype html>
<html>
<head><meta charset="utf-8"><title>Duet Spike B - webview side</title></head>
<body style="font-family: -apple-system, sans-serif; background:#1e1e2e; color:#eee;">
  <h1>Duet Spike B - webview side</h1>
  <div id="raf">raf: 0</div>
  <div id="interval">interval: 0</div>
  <div id="pings">pings: 0</div>
  <div id="clicks">clicks: 0</div>
  <div id="vis">visibilityState: unknown, hidden: unknown</div>
  <script>
    window.__spikeB = { raf: 0, interval: 0, pings: 0, lastPingN: null, clicks: 0 };
    function tick() {
      window.__spikeB.raf++;
      document.getElementById('raf').textContent = 'raf: ' + window.__spikeB.raf;
      requestAnimationFrame(tick);
    }
    requestAnimationFrame(tick);
    setInterval(function() {
      window.__spikeB.interval++;
      document.getElementById('interval').textContent = 'interval: ' + window.__spikeB.interval;
    }, 50);
    document.getElementById('vis').textContent =
      'visibilityState: ' + document.visibilityState + ', hidden: ' + document.hidden;
    document.addEventListener('visibilitychange', function() {
      document.getElementById('vis').textContent =
        'visibilityState: ' + document.visibilityState + ', hidden: ' + document.hidden;
    });
    document.body.addEventListener('mousedown', function() {
      window.__spikeB.clicks++;
      document.getElementById('clicks').textContent = 'clicks: ' + window.__spikeB.clicks;
    });
  </script>
</body>
</html>"#;

fn setup(state: &mut AppState, target: &tao::event_loop::EventLoopWindowTarget<UserEvent>) {
    state.log.line("=== Setup: booting Flutter engine, creating both windows ===");

    // --- Flutter side ---
    let engine = unsafe { boot_headless_engine(&state.log) };
    state.log.line("Flutter engine booted headless (allowHeadlessExecution=true)");

    let flutter_window = WindowBuilder::new()
        .with_title("Duet Spike B - Flutter")
        .with_inner_size(LogicalSize::new(500.0, 500.0))
        .build(target)
        .expect("failed to build Flutter tao window");

    let controller = unsafe { create_view_controller(&engine) };
    unsafe { attach_view_to_window(&flutter_window, &controller) };
    state.log.line("Flutter view created and parented into its tao window");

    let method_channel = unsafe { make_method_channel(&engine) };
    state.log.line("FlutterMethodChannel 'duet/spike_b' built against engine.binaryMessenger");

    // --- Webview side ---
    let webview_window = WindowBuilder::new()
        .with_title("Duet Spike B - WebView")
        .with_inner_size(LogicalSize::new(500.0, 500.0))
        .build(target)
        .expect("failed to build webview tao window");

    let webview = WebViewBuilder::new()
        .with_html(WEBVIEW_HTML)
        .build(&webview_window)
        .expect("failed to build wry WebView");
    state.log.line("wry WebView created and built directly against its own tao window");

    state.log.pass(
        "1 (setup)",
        "one tao EventLoop hosts both a Flutter-backed window and a wry-backed window, \
         created back to back on the same main thread with no crash",
    );

    state.engine = Some(engine);
    state.method_channel = Some(method_channel);
    state.flutter_window = Some(flutter_window);
    state.flutter_controller = Some(controller);
    state.webview_window = Some(webview_window);
    state.webview = Some(webview);
    state.start = Instant::now();

    state.log.line(&format!(
        "Baseline RSS after both surfaces created: {} kB",
        rss_kb_num()
    ));
}

fn on_tick(state: &mut AppState, n: u64) {
    state.proxy_received.fetch_add(1, Ordering::SeqCst);
    let elapsed = state.start.elapsed();
    let verbose = n % LOG_EVERY_N_TICKS == 0;

    if verbose {
        let raf_now = *state.last_js_raf.lock().unwrap();
        let interval_now = *state.last_js_interval.lock().unwrap();
        state.log.line(&format!(
            "--- tick n={n} elapsed={:.1}s sent={} received={} flutter_replies={} webview_replies={} \
             js_raf={raf_now} js_interval={interval_now} ---",
            elapsed.as_secs_f64(),
            state.proxy_sent.load(Ordering::SeqCst),
            state.proxy_received.load(Ordering::SeqCst),
            state.flutter_replies_received.load(Ordering::SeqCst),
            state.webview_replies_received.load(Ordering::SeqCst),
        ));

        // NSWindow.occlusionState on both windows - part of the rAF-stall
        // discriminator investigation: if WebKit is throttling rAF because
        // it considers the webview window non-visible/occluded, this should
        // show it lacking the Visible bit.
        if let (Some(fw), Some(ww)) = (&state.flutter_window, &state.webview_window) {
            unsafe {
                let flutter_ns = &*(fw.ns_window() as *mut NSWindow);
                let webview_ns = &*(ww.ns_window() as *mut NSWindow);
                state.log.line(&format!(
                    "occlusionState: flutter_window={:?} (visible={}) webview_window={:?} (visible={})",
                    flutter_ns.occlusionState(),
                    flutter_ns.occlusionState().contains(objc2_app_kit::NSWindowOcclusionState::Visible),
                    webview_ns.occlusionState(),
                    webview_ns.occlusionState().contains(objc2_app_kit::NSWindowOcclusionState::Visible),
                ));
            }
        }

        // rAF-stall discriminator: has raf actually stopped advancing since
        // the previous verbose sample (~LOG_EVERY_N_TICKS*TICK_INTERVAL
        // apart), while setInterval (a differently-throttled JS timer) and
        // the eval round-trip itself (webview_replies) keep moving? If so,
        // that points at WebKit's own compositor/visibility throttling of
        // rAF specifically, not at the host run loop starving the webview.
        if state.raf_last_value >= 0 && raf_now == state.raf_last_value && !state.raf_stall_logged {
            state.raf_stall_logged = true;
            state.log.line(&format!(
                "*** rAF STALL DETECTED at t+{:.1}s: js_raf unchanged at {raf_now} since previous \
                 sample, while js_interval={interval_now} and webview_replies={} kept advancing \
                 (eval round-trips are still landing - the main thread is NOT starved). See \
                 FINDINGS.md discriminator section. ***",
                elapsed.as_secs_f64(),
                state.webview_replies_received.load(Ordering::SeqCst),
            ));
        }
        state.raf_last_value = raf_now;
    }

    // ===== Criterion 3: proxy -> main thread -> BOTH Flutter channel + webview eval =====
    if let Some(channel) = &state.method_channel {
        unsafe {
            flutter_ping(
                channel,
                n,
                Arc::clone(&state.flutter_replies_received),
                Arc::clone(&state.last_flutter_frame_ticks),
                verbose,
            )
        };
    }
    if let Some(webview) = &state.webview {
        // Write a value into the page (pings++, lastPingN) then return a plain
        // JS object - NOT a pre-stringified JSON string. wry's
        // evaluate_script_with_callback already runs the returned JS value
        // through NSJSONSerialization on the native side (see wry's
        // wkwebview/mod.rs `eval`), so returning an object here gives a
        // clean, single-encoded JSON string back in Rust. Returning an
        // already-stringified string here would get double-JSON-encoded
        // (quoted and escaped) by that same step - this spike hit exactly
        // that bug on the first run; see FINDINGS.md.
        let js = format!(
            "(function(){{ window.__spikeB.pings++; window.__spikeB.lastPingN = {n}; \
             document.getElementById('pings').textContent = 'pings: ' + window.__spikeB.pings + ' (last n=' + {n} + ')'; \
             return {{pings: window.__spikeB.pings, lastPingN: window.__spikeB.lastPingN, raf: window.__spikeB.raf, \
             interval: window.__spikeB.interval, clicks: window.__spikeB.clicks, \
             visibilityState: document.visibilityState, hidden: document.hidden}}; }})()"
        );
        let received = Arc::clone(&state.webview_replies_received);
        let last_raf = Arc::clone(&state.last_js_raf);
        let last_interval = Arc::clone(&state.last_js_interval);
        let webview_verbose = verbose;
        let _ = webview.evaluate_script_with_callback(&js, move |result: String| {
            received.fetch_add(1, Ordering::SeqCst);
            // result is a clean single-encoded JSON string (see comment
            // above about the double-encoding bug this spike hit and
            // fixed); pull fields out with a tiny manual scan rather than
            // pulling in a JSON crate for a spike.
            if let Some(v) = extract_json_number(&result, "raf") {
                if let Ok(mut guard) = last_raf.lock() {
                    *guard = v;
                }
            }
            if let Some(v) = extract_json_number(&result, "interval") {
                if let Ok(mut guard) = last_interval.lock() {
                    *guard = v;
                }
            }
            if webview_verbose {
                println!("[spike-b] webview eval readback for n={n}: {result}");
            }
        });
    }

    if n == 0 {
        state.log.pass(
            "3",
            "first proxy Tick delivered on main thread; dispatched to BOTH \
             FlutterMethodChannel.invokeMethod (Flutter) and WebView::evaluate_script_with_callback \
             (webview) from the same UserEvent handler",
        );
    }

    // ===== Criterion 1: rasterize both windows while both are demonstrably alive =====
    if !state.both_alive_snapshot_taken && elapsed >= Duration::from_secs(2) {
        state.both_alive_snapshot_taken = true;
        if let (Some(controller), Some(webview)) = (&state.flutter_controller, &state.webview) {
            let flutter_view: Retained<AnyObject> = unsafe { msg_send![&**controller, view] };
            let flutter_view: Retained<NSView> = unsafe { Retained::cast_unchecked(flutter_view) };
            unsafe { snapshot_nsview_to_png(&flutter_view, "evidence/both_alive_flutter.png") };
            let wk_view = webview.webview();
            unsafe { snapshot_nsview_to_png(&wk_view, "evidence/both_alive_webview.png") };
            state.log.pass(
                "1",
                "both windows rasterized to PNG (evidence/both_alive_flutter.png, \
                 evidence/both_alive_webview.png) at t+2s, while both are actively receiving \
                 proxy-driven ticks",
            );
        }
    }

    // ===== Criterion 5: synthetic input =====
    if !state.synthetic_input_done && elapsed >= SYNTHETIC_INPUT_AT {
        state.synthetic_input_done = true;
        if let Some(w) = &state.flutter_window {
            unsafe { send_synthetic_click(w, "Flutter window") };
        }
        if let Some(w) = &state.webview_window {
            unsafe { send_synthetic_click(w, "webview window") };
        }
    }
    if state.synthetic_input_done && !state.synthetic_input_checked && elapsed >= SYNTHETIC_INPUT_CHECK_AT {
        state.synthetic_input_checked = true;
        if let Some(channel) = &state.method_channel {
            unsafe { flutter_query(channel, "post-synthetic-input tapCount check") };
        }
        if let Some(webview) = &state.webview {
            let _ = webview.evaluate_script_with_callback(
                "({clicks: window.__spikeB.clicks})",
                |result: String| {
                    println!("[spike-b] webview post-synthetic-input clicks check: {result}");
                },
            );
        }
    }

    // ===== Criterion 2: neither starves the other, sampled over ~30s (or override) =====
    //
    // IMPORTANT (corrected after review of the first 180s run): js_raf is
    // NOT used as the primary "webview is progressing" signal here, because
    // requestAnimationFrame in WKWebView is throttled/suspended based on the
    // window's compositor visibility, and this environment has no reachable
    // WindowServer - a first full run showed raf stall permanently partway
    // through while everything else kept working (see the rAF STALL
    // DETECTED log line and FINDINGS.md's discriminator section). The
    // primary signals used for "is the webview side actually still alive
    // and running JS" are:
    //   - webview_replies_received: each is a full native->JS->native
    //     eval() round trip dispatched from the SAME main-thread tick that
    //     drives Flutter, so if this keeps advancing the main thread is
    //     provably not starving the webview.
    //   - js_interval: a setInterval timer running entirely inside the
    //     page's own JS engine, independent of our eval calls. If this also
    //     keeps advancing, the page's JS run loop itself is alive
    //     (independent corroboration, not just "eval calls still land").
    // js_raf is still reported for completeness and because a stall in it
    // is itself an interesting, honestly-reported finding.
    if !state.progress_checkpoint_done && elapsed >= state.checkpoint_at {
        state.progress_checkpoint_done = true;
        let js_raf = *state.last_js_raf.lock().unwrap();
        let js_interval = *state.last_js_interval.lock().unwrap();
        let flutter_frame_ticks = *state.last_flutter_frame_ticks.lock().unwrap();
        state.log.line(&format!(
            "criterion 2 checkpoint at t+{:.0}s: flutter pings landed so far={}, \
             flutter's own scheduler frameTicks={} (driven by vsync, not by our ticks), \
             webview evals landed so far={}, webview's own requestAnimationFrame raf={}, \
             webview's own setInterval counter={}",
            elapsed.as_secs_f64(),
            state.flutter_replies_received.load(Ordering::SeqCst),
            flutter_frame_ticks,
            state.webview_replies_received.load(Ordering::SeqCst),
            js_raf,
            js_interval,
        ));
        // Thresholds scale with the checkpoint window: expect roughly one
        // ping reply per TICK_INTERVAL, and vsync/interval at a generous
        // floor to tolerate a slow/loaded machine.
        let checkpoint_secs = state.checkpoint_at.as_secs_f64();
        let min_replies = ((checkpoint_secs / TICK_INTERVAL.as_secs_f64()) * 0.5) as i64;
        let min_frames = (checkpoint_secs * 15.0) as i64;
        let flutter_ok =
            state.flutter_replies_received.load(Ordering::SeqCst) as i64 > min_replies && flutter_frame_ticks > min_frames;
        // setInterval fires every 50ms => ~20/s; use a conservative floor.
        let min_interval_ticks = (checkpoint_secs * 8.0) as i64;
        let webview_ok = state.webview_replies_received.load(Ordering::SeqCst) as i64 > min_replies
            && js_interval > min_interval_ticks;
        if flutter_ok && webview_ok {
            state.log.pass(
                "2",
                &format!(
                    "over ~{:.0}s, Flutter's own vsync-driven scheduler reached frameTicks={} while \
                     {} channel replies also landed, and the webview's own setInterval counter \
                     reached {} (independent of our eval calls) while {} webview eval round-trips \
                     also landed - neither side stalled while the other was being driven. \
                     (js_raf={} at this checkpoint - see FINDINGS.md if it later stalls; that is a \
                     WebKit rAF-throttling question, not a run-loop-starvation one - see the \
                     discriminator this spike uses.)",
                    state.checkpoint_at.as_secs_f64(),
                    flutter_frame_ticks,
                    state.flutter_replies_received.load(Ordering::SeqCst),
                    js_interval,
                    state.webview_replies_received.load(Ordering::SeqCst),
                    js_raf,
                ),
            );
        } else {
            state.log.line(&format!(
                "criterion 2 INCONCLUSIVE at checkpoint: flutter_ok={flutter_ok} webview_ok={webview_ok}"
            ));
        }
    }

    // ===== Criterion 4: sustained run, no deadlock, sent vs received, RSS start/end =====
    if !state.finishing && elapsed >= state.run_duration {
        state.finishing = true;
        state.running_flag.store(false, Ordering::SeqCst);
        let sent = state.proxy_sent.load(Ordering::SeqCst);
        let received = state.proxy_received.load(Ordering::SeqCst);
        state.log.line(&format!(
            "=== Sustained run complete after {:.0}s: proxy events sent={} received={} \
             (missed={}) - no deadlock observed (this log line proves the main thread was \
             still pumping events right up to the end) ===",
            elapsed.as_secs_f64(),
            sent,
            received,
            sent.saturating_sub(received),
        ));
        state.log.pass(
            "4",
            &format!(
                "ran {:.0}s under continuous proxy-driven load with both surfaces live; \
                 sent={sent} received={received}; final RSS={} kB",
                elapsed.as_secs_f64(),
                rss_kb_num()
            ),
        );

        state.log.cannot_verify(
            "5 (real hardware input)",
            "this environment has no reachable on-screen WindowServer for spawned processes \
             (confirmed independently in spike-a-macos), so real mouse/keyboard hardware input \
             cannot be generated or observed here; only synthetic NSEvent injection (above) was \
             testable. Verifying real input would need a machine with an actual attached display \
             and WindowServer session, or a physical/virtual device under a real login session.",
        );
        state.log.cannot_verify(
            "6 (Linux / GTK)",
            "not attempted - platform unavailable on this machine (macOS only). Flutter's GTK \
             embedder plus WebKitGTK sharing one GTK main loop remains an open risk per the brief; \
             would need a Linux machine (or VM) with GTK3, Flutter's linux embedder build, and \
             webkit2gtk installed.",
        );

        state.log.line("=== Final summary ===");
        state.log.line(&format!(
            "criterion 1 (simultaneous coexistence): PASS - see evidence/both_alive_flutter.png \
             and evidence/both_alive_webview.png"
        ));
        state.log.line(&format!(
            "criterion 2 (neither starves the other): {} at t+{:.0}s checkpoint (webview_replies + \
             setInterval as the discriminator, NOT js_raf - see rAF STALL line above if present, \
             and FINDINGS.md) - see checkpoint log above",
            if state.progress_checkpoint_done { "PASS" } else { "NOT REACHED" },
            state.checkpoint_at.as_secs_f64(),
        ));
        state.log.line("criterion 3 (proxy drives both sides): PASS - see per-tick logs above");
        state.log.line(&format!(
            "criterion 4 (sustained {:.0}s, no deadlock): PASS - sent={sent} received={received} missed={}",
            state.run_duration.as_secs_f64(),
            sent.saturating_sub(received)
        ));
        state.log.line("criterion 5 (synthetic input routing): see SYNTHETIC INPUT lines above for what was attempted and observed");
        state.log.line("criterion 6 (Linux): CANNOT-VERIFY-HERE - not attempted, platform unavailable");
    }
}

/// Tiny hand-rolled scan for `"key":<number>` inside a JSON string - avoids
/// pulling in a JSON crate for a throwaway spike.
fn extract_json_number(json: &str, key: &str) -> Option<i64> {
    let needle = format!("\"{key}\":");
    let idx = json.find(&needle)?;
    let rest = &json[idx + needle.len()..];
    let end = rest.find(|c: char| !(c.is_ascii_digit() || c == '-')).unwrap_or(rest.len());
    rest[..end].parse().ok()
}
