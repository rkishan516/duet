//! Spike C: prove (or disprove) that a Rust-hosted Flutter engine in JIT mode
//! can be hot-reloaded via the Dart VM service, driven by a `frontend_server`
//! we manage ourselves - and measure the real edit-to-pixel latency. See
//! FINDINGS.md for the writeup; this is throwaway spike code (minimal error
//! handling beyond panics, liberal logging, no tests - see the brief).
//!
//! IMPORTANT: this process redirects its own fd 1 (stdout) early (see
//! `stdout_capture`) in order to programmatically recover the Dart VM
//! service URI the engine prints. From that point on, all of THIS process's
//! own diagnostic output goes through `eprintln!` (stderr), never
//! `println!`/`print!`.

mod flutter_embed;
mod frontend_server;
mod stdout_capture;
mod vm_service;

use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use objc2::rc::Retained;
use objc2::runtime::AnyObject;

use tao::dpi::LogicalSize;
use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoop};
use tao::window::{Window, WindowBuilder};

use frontend_server::{FrontendServer, FrontendServerConfig};
use vm_service::VmServiceClient;

// ===================== Fixed environment paths (verified in the brief) =====================

const FLUTTER_ROOT: &str = "/Users/kishan/dev/rkishan516/flutterDC";
const SPIKE_APP_DIR: &str = "/Users/kishan/dev/rkishan516/tauri-flutter/spikes/spike_app";

fn app_framework_path() -> String {
    std::env::var("DUET_APP_FRAMEWORK_PATH").unwrap_or_else(|_| {
        format!("{SPIKE_APP_DIR}/build/macos/Build/Products/Debug/App.framework")
    })
}
fn main_dart_path() -> String {
    format!("{SPIKE_APP_DIR}/lib/main.dart")
}
/// The `.dart_tool/package_config.json` maps package `spike_app` -> `lib/`
/// (rootUri `../`, packageUri `lib/`), so this resolves to the same file as
/// `main_dart_path()`. flutter_tools' resident compiler identifies the
/// entrypoint (and invalidated files, when resolvable) via `package:` URIs,
/// not raw `file://` paths - see FINDINGS.md for why using `file://`
/// instead crashed the VM's reload with "Class: StatelessWidget which is
/// not loaded yet": the root library's identity inside the compiled kernel
/// needs to match how the rest of the framework's kernel already identifies
/// libraries, and mixing URI schemes for "the same" library breaks that.
const ENTRYPOINT_PACKAGE_URI: &str = "package:spike_app/main.dart";
fn dartaotruntime_path() -> String {
    format!("{FLUTTER_ROOT}/bin/cache/dart-sdk/bin/dartaotruntime")
}
fn frontend_server_snapshot_path() -> String {
    format!("{FLUTTER_ROOT}/bin/cache/dart-sdk/bin/snapshots/frontend_server_aot.dart.snapshot")
}
fn sdk_root_path() -> String {
    // Trailing slash required - frontend_server silently misbehaves without it.
    format!("{FLUTTER_ROOT}/bin/cache/artifacts/engine/common/flutter_patched_sdk/")
}
fn package_config_path() -> String {
    format!("{SPIKE_APP_DIR}/.dart_tool/package_config.json")
}

fn iterations() -> usize {
    std::env::var("DUET_SPIKE_C_ITERATIONS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(5)
}
fn per_iteration_wait() -> Duration {
    Duration::from_secs(
        std::env::var("DUET_SPIKE_C_WAIT_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(5),
    )
}

const TAP_TARGET: i64 = 3;
const TICK_INTERVAL: Duration = Duration::from_millis(100);

// ===================== Logging =====================

struct Log {
    start: Instant,
}
impl Log {
    fn new() -> Self {
        Self { start: Instant::now() }
    }
    fn line(&self, msg: &str) {
        eprintln!("[t+{:>7.3}s] {msg}", self.start.elapsed().as_secs_f64());
    }
}

fn rss_kb() -> String {
    let pid = std::process::id().to_string();
    match Command::new("ps").args(["-o", "rss=", "-p", &pid]).output() {
        Ok(out) => String::from_utf8_lossy(&out.stdout).trim().to_string(),
        Err(_) => "?".to_string(),
    }
}

// ===================== Shared state between main thread and reload-driver thread =====================

#[derive(Clone, Debug, Default)]
struct FrameReport {
    marker: String,
    tap_count: i64,
}

#[derive(Debug)]
struct Sample {
    iteration: usize,
    marker: String,
    recompile_ok: bool,
    recompile_diagnostics: Vec<String>,
    reload_response: Option<serde_json::Value>,
    reassemble_response: Option<serde_json::Value>,
    observed: bool,
    observed_tap_count: Option<i64>,
    latency: Duration,
}

struct SharedResults {
    samples: Mutex<Vec<Sample>>,
    done: AtomicBool,
    baseline_tap_count: Mutex<i64>,
}

/// Rewrites the single `const String kReloadMarker = '...';` literal in
/// main.dart in place. Deterministic regardless of the CURRENT value, so the
/// reload loop doesn't need to track it separately.
fn set_marker_in_file(path: &str, new_marker: &str) -> String {
    let content = std::fs::read_to_string(path).expect("failed to read main.dart");
    let needle = "const String kReloadMarker = '";
    let start = content
        .find(needle)
        .unwrap_or_else(|| panic!("kReloadMarker const declaration not found in {path}"))
        + needle.len();
    let end = start
        + content[start..]
            .find('\'')
            .unwrap_or_else(|| panic!("unterminated kReloadMarker string literal in {path}"));
    let mut new_content = String::with_capacity(content.len());
    new_content.push_str(&content[..start]);
    new_content.push_str(new_marker);
    new_content.push_str(&content[end..]);
    new_content
}

/// Runs entirely on a background OS thread: no ObjC/AppKit calls happen
/// here, only file I/O, a child process's pipes, and a plain TCP/WebSocket.
/// This is deliberate - see FINDINGS.md's threading discussion: the main
/// thread must keep pumping the CFRunLoop (tao's event loop) for Flutter's
/// merged UI/platform thread to make progress applying each reload, so
/// nothing on this thread may block that pump. Blocking calls here only
/// block THIS thread, which is fine.
fn run_reload_loop(
    mut fs: FrontendServer,
    mut vm: VmServiceClient,
    isolate_id: String,
    frame_state: Arc<Mutex<FrameReport>>,
    results: Arc<SharedResults>,
    log_start: Instant,
) {
    let log = |msg: &str| eprintln!("[t+{:>7.3}s][reload-thread] {msg}", log_start.elapsed().as_secs_f64());

    let main_dart = main_dart_path();
    let n = iterations();
    let wait_budget = per_iteration_wait();

    log(&format!("starting reload loop: {n} iterations, per-iteration wait budget {wait_budget:?}"));

    for i in 0..n {
        let new_marker = format!("MARKER_V{}", i + 2); // V1 is the baseline already loaded.
        let new_content = set_marker_in_file(&main_dart, &new_marker);

        // Clock starts here: "source file written" per the brief, not the RPC round trip.
        let t0 = Instant::now();
        std::fs::write(&main_dart, new_content).expect("failed to write main.dart");

        let (recompile_result, incremental_dill) = fs.recompile(ENTRYPOINT_PACKAGE_URI);
        log(&format!(
            "iter {i}: recompile -> ok={} elapsed={:?} diagnostics={:?}",
            recompile_result.ok, recompile_result.elapsed, recompile_result.diagnostic_lines
        ));

        let mut reload_response = None;
        let mut reassemble_response = None;
        if recompile_result.ok {
            fs.accept();
            let dill_uri = format!("file://{incremental_dill}");
            let resp = vm.reload_sources(&isolate_id, &dill_uri);
            log(&format!("iter {i}: reloadSources response = {resp}"));
            let reload_ok = resp
                .get("result")
                .and_then(|r| r.get("success"))
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            reload_response = Some(resp);
            if reload_ok {
                let rresp = vm.reassemble(&isolate_id);
                log(&format!("iter {i}: ext.flutter.reassemble response = {rresp}"));
                reassemble_response = Some(rresp);
            } else {
                log(&format!("iter {i}: reloadSources did NOT report success, skipping reassemble"));
            }
        } else {
            log(&format!("iter {i}: recompile failed/had errors, skipping reloadSources"));
        }

        // Poll for the NEW marker to arrive over the platform channel - this is
        // "the changed UI has actually rendered a frame", not just the RPC finishing.
        let deadline = Instant::now() + wait_budget;
        let mut observed = false;
        let mut observed_tap_count = None;
        while Instant::now() < deadline {
            {
                let fr = frame_state.lock().unwrap();
                if fr.marker == new_marker {
                    observed = true;
                    observed_tap_count = Some(fr.tap_count);
                    break;
                }
            }
            thread::sleep(Duration::from_millis(2));
        }
        let latency = t0.elapsed();
        log(&format!(
            "iter {i}: marker={new_marker} observed={observed} latency={latency:?} tap_count={observed_tap_count:?}"
        ));

        results.samples.lock().unwrap().push(Sample {
            iteration: i,
            marker: new_marker,
            recompile_ok: recompile_result.ok,
            recompile_diagnostics: recompile_result.diagnostic_lines,
            reload_response,
            reassemble_response,
            observed,
            observed_tap_count,
            latency,
        });
    }

    log("reload loop finished, restoring main.dart to MARKER_V1 baseline");
    let restored = set_marker_in_file(&main_dart, "MARKER_V1");
    std::fs::write(&main_dart, restored).expect("failed to restore main.dart");

    fs.quit();
    results.done.store(true, Ordering::SeqCst);
}

// ===================== App state / event loop =====================

struct AppState {
    log: Log,
    engine: Option<Retained<AnyObject>>,
    window: Option<Window>,
    controller: Option<Retained<AnyObject>>,
    channel_b: Option<Retained<AnyObject>>,
    frame_state: Arc<Mutex<FrameReport>>,
    results: Arc<SharedResults>,
    reload_thread_started: bool,
    before_snapshot_taken: bool,
    finalized: bool,
    finishing: bool,
    increments_sent: i64,
    last_increment_send: Option<Instant>,
    /// Frontend server + VM service client + isolate id, built during setup()
    /// and handed off to the background reload-driver thread once tapCount
    /// priming is confirmed (see tick()).
    pending: Option<(FrontendServer, VmServiceClient, String)>,
}

fn main() {
    // MUST happen before anything else prints to stdout, and before booting the engine.
    let stdout_rx = stdout_capture::capture_stdout_lines();

    let log = Log::new();
    log.line("Spike C starting: hot reload via frontend_server + Dart VM service");
    log.line(&format!("App.framework path: {}", app_framework_path()));
    log.line(&format!("main.dart path: {}", main_dart_path()));

    // Defensive: make sure main.dart starts at the known baseline before we touch anything.
    let baseline = set_marker_in_file(&main_dart_path(), "MARKER_V1");
    std::fs::write(main_dart_path(), baseline).expect("failed to reset main.dart to MARKER_V1 baseline");

    let event_loop: EventLoop<()> = EventLoop::new();

    let mut state = AppState {
        log,
        engine: None,
        window: None,
        controller: None,
        channel_b: None,
        frame_state: Arc::new(Mutex::new(FrameReport::default())),
        results: Arc::new(SharedResults {
            samples: Mutex::new(Vec::new()),
            done: AtomicBool::new(false),
            baseline_tap_count: Mutex::new(-1),
        }),
        reload_thread_started: false,
        before_snapshot_taken: false,
        finalized: false,
        finishing: false,
        increments_sent: 0,
        last_increment_send: None,
        pending: None,
    };

    event_loop.run(move |event, target, control_flow| match event {
        Event::NewEvents(StartCause::Init) => {
            setup(&mut state, target, &stdout_rx);
            *control_flow = ControlFlow::WaitUntil(Instant::now() + TICK_INTERVAL);
        }
        Event::NewEvents(StartCause::ResumeTimeReached { .. }) => {
            tick(&mut state);
            if state.finishing {
                *control_flow = ControlFlow::Exit;
            } else {
                *control_flow = ControlFlow::WaitUntil(Instant::now() + TICK_INTERVAL);
            }
        }
        Event::LoopDestroyed => {
            state.log.line("LoopDestroyed - process exiting");
        }
        _ => {}
    });
}

fn setup(state: &mut AppState, target: &tao::event_loop::EventLoopWindowTarget<()>, stdout_rx: &std::sync::mpsc::Receiver<String>) {
    // Start our own, fully independent frontend_server session and give it a
    // full baseline compile before booting the engine. The engine itself
    // boots from the app bundle's EXISTING kernel_blob.bin (the one Spike A
    // already proved works, produced by a separate, ordinary
    // `flutter build macos --debug`) - this frontend_server session's own
    // baseline compile is used ONLY to prime its internal incremental-
    // compiler state, never fed to the engine directly.
    //
    // This spike originally suspected the two compiles (bundle's kernel vs.
    // this session's baseline) needed to be the EXACT SAME kernel instance,
    // and spliced this session's baseline dill over kernel_blob.bin to force
    // that. That turned out to be unnecessary - seeSpike C's FINDINGS.md,
    // "false start" section - the app boots fine from either kernel and
    // reload still works, PROVIDED reloadSources is NOT called with
    // `force: true` (see vm_service.rs for that finding, which was the
    // actual root cause of an early VM-fatal crash during this spike).
    let work_dir = format!("/tmp/spike-c-frontend-server-{}", std::process::id());
    std::fs::create_dir_all(&work_dir).expect("failed to create frontend_server work dir");
    let output_dill = format!("{work_dir}/out.dill");

    let dartaotruntime = dartaotruntime_path();
    let snapshot = frontend_server_snapshot_path();
    let sdk_root = sdk_root_path();
    let packages = package_config_path();
    let cfg = FrontendServerConfig {
        dartaotruntime: &dartaotruntime,
        snapshot: &snapshot,
        sdk_root: &sdk_root,
        packages: &packages,
        output_dill: &output_dill,
    };
    state.log.line(&format!(
        "spawning frontend_server: {dartaotruntime} {snapshot} --sdk-root {sdk_root} --incremental \
         --track-widget-creation --experimental-emit-debug-metadata --target=flutter \
         --no-print-incremental-dependencies --packages {packages} --output-dill {output_dill}"
    ));
    let mut frontend = FrontendServer::spawn(&cfg);

    let baseline_compile = frontend.compile(ENTRYPOINT_PACKAGE_URI);
    state.log.line(&format!(
        "initial baseline compile -> ok={} elapsed={:?} diagnostics={:?}",
        baseline_compile.ok, baseline_compile.elapsed, baseline_compile.diagnostic_lines
    ));
    assert!(baseline_compile.ok, "baseline compile failed, cannot proceed");
    frontend.accept();
    state.log.line("baseline compile accepted; frontend_server ready for incremental recompiles");

    state.log.line("=== booting Flutter engine (from the app bundle's existing kernel_blob.bin) ===");
    let engine = unsafe { flutter_embed::boot_headless_engine(&app_framework_path(), "duet-spike-c") };
    state.log.line("engine booted (allowHeadlessExecution=true, runWithEntrypoint returned true)");

    let vm_service_base = stdout_capture::wait_for_vm_service_uri(stdout_rx, Duration::from_secs(15))
        .expect("did not observe 'The Dart VM service is listening on ...' within 15s");
    state.log.line(&format!("VM service base URL: {vm_service_base}"));

    let window = WindowBuilder::new()
        .with_title("Duet Spike C - hot reload")
        .with_inner_size(LogicalSize::new(500.0, 500.0))
        .build(target)
        .expect("failed to build tao window");

    let controller = unsafe { flutter_embed::create_view_controller(&engine) };
    unsafe { flutter_embed::attach_view_to_window(&window, &controller) };
    state.log.line("Flutter view created and parented into tao window");

    let channel_c = unsafe { flutter_embed::make_method_channel(&engine, "duet/spike_c") };
    let frame_state = Arc::clone(&state.frame_state);
    unsafe {
        flutter_embed::register_frame_marker_handler(&channel_c, move |marker, tap_count| {
            let mut fr = frame_state.lock().unwrap();
            fr.marker = marker;
            fr.tap_count = tap_count;
        });
    }
    state.log.line("registered native handler for duet/spike_c 'frameMarker' calls");

    // NOTE: 'increment' calls are deliberately NOT fired here yet. Flutter's
    // platform channel messenger only buffers ONE pending message per channel
    // until a Dart-side handler is registered (Dart's widget tree/initState
    // hasn't necessarily run yet immediately after runWithEntrypoint returns) -
    // firing all of them this early silently dropped all but the last one in
    // an earlier version of this spike (observed: 3 sent, only 1 landed). See
    // FINDINGS.md. Instead, tick() fires (and retries) 'increment' calls only
    // once the very first frameMarker report has been observed, proving Dart's
    // handler is registered and listening.
    let channel_b = unsafe { flutter_embed::make_method_channel(&engine, "duet/spike_b") };
    state.channel_b = Some(channel_b);

    // VM service connection + frontend_server startup are both blocking I/O against
    // things OTHER than our own main thread's run loop (a TCP socket, a child
    // process) - safe to do here in setup(), which still runs cooperatively with
    // tao's event loop machinery already active.
    let ws_url = vm_service::http_to_ws_url(&vm_service_base);
    state.log.line(&format!("connecting to VM service websocket: {ws_url}"));
    let mut vm_client = VmServiceClient::connect(&ws_url);
    let vm_response = vm_client.get_vm();
    state.log.line(&format!("getVM response: {vm_response}"));
    let isolate_id = vm_response
        .get("result")
        .and_then(|r| r.get("isolates"))
        .and_then(|i| i.as_array())
        .and_then(|arr| arr.first())
        .and_then(|iso| iso.get("id"))
        .and_then(|id| id.as_str())
        .expect("could not find isolates[0].id in getVM response")
        .to_string();
    state.log.line(&format!("isolate id: {isolate_id}"));

    state.engine = Some(engine);
    state.window = Some(window);
    state.controller = Some(controller);

    // Stash everything the reload-driver thread needs; it's spawned once tapCount
    // priming is confirmed (see tick()).
    state.pending = Some((frontend, vm_client, isolate_id));
}

fn tick(state: &mut AppState) {
    if state.reload_thread_started {
        if !state.before_snapshot_taken {
            // handled at spawn time, nothing to do
        }
        if state.results.done.load(Ordering::SeqCst) && !state.finalized {
            finalize(state);
        }
        return;
    }

    let fr = state.frame_state.lock().unwrap().clone();

    if fr.marker.is_empty() {
        return; // no frameMarker report observed yet - Dart side isn't ready.
    }

    let tap_count_now = fr.tap_count;
    if tap_count_now < TAP_TARGET {
        // Send (or resend) 'increment' calls to make up the deficit. Flutter's
        // platform channel messenger only buffers ONE pending message per
        // channel until a Dart-side handler is registered; firing all of them
        // before the first frameMarker report was observed silently dropped all
        // but one in an earlier version of this spike (see FINDINGS.md). Now
        // that we've seen a report, the handler is definitely registered, but
        // we still retry with a backoff in case of any other transient loss.
        let should_send = match state.last_increment_send {
            None => true,
            Some(t) => t.elapsed() > Duration::from_millis(500),
        };
        if should_send {
            let deficit = TAP_TARGET - tap_count_now;
            if let Some(channel_b) = &state.channel_b {
                for _ in 0..deficit {
                    unsafe { flutter_embed::invoke_no_args(channel_b, "increment") };
                }
            }
            state.increments_sent += deficit;
            state.last_increment_send = Some(Instant::now());
            state.log.line(&format!(
                "priming: tapCount={tap_count_now}/{TAP_TARGET}, sent {deficit} more 'increment' call(s) \
                 (total attempts sent so far: {})",
                state.increments_sent
            ));
        }
        return; // keep waiting for the increments to land
    }

    state.log.line(&format!(
        "tapCount primed to {tap_count_now} (target {TAP_TARGET}) - Dart heap state is non-zero, proceeding"
    ));
    *state.results.baseline_tap_count.lock().unwrap() = tap_count_now;

    if let Some(controller) = &state.controller {
        let flutter_view: Retained<AnyObject> = unsafe { objc2::msg_send![&**controller, view] };
        let flutter_view: Retained<objc2_app_kit::NSView> = unsafe { Retained::cast_unchecked(flutter_view) };
        std::fs::create_dir_all("evidence").ok();
        unsafe { flutter_embed::snapshot_nsview_to_png(&flutter_view, "evidence/before_reload.png") };
    }
    state.before_snapshot_taken = true;

    let (frontend, vm_client, isolate_id) = state.pending.take().expect("pending frontend_server/vm_client missing");
    let frame_state = Arc::clone(&state.frame_state);
    let results = Arc::clone(&state.results);
    let log_start = state.log.start;
    thread::Builder::new()
        .name("spike-c-reload-driver".into())
        .spawn(move || run_reload_loop(frontend, vm_client, isolate_id, frame_state, results, log_start))
        .expect("failed to spawn reload-driver thread");
    state.reload_thread_started = true;
    state.log.line("reload-driver thread spawned");
}

fn finalize(state: &mut AppState) {
    state.finalized = true;
    state.log.line("=== reload loop complete, finalizing ===");

    if let Some(controller) = &state.controller {
        let flutter_view: Retained<AnyObject> = unsafe { objc2::msg_send![&**controller, view] };
        let flutter_view: Retained<objc2_app_kit::NSView> = unsafe { Retained::cast_unchecked(flutter_view) };
        unsafe { flutter_embed::snapshot_nsview_to_png(&flutter_view, "evidence/after_reload.png") };
    }

    let samples = state.results.samples.lock().unwrap();
    let baseline_tap = *state.results.baseline_tap_count.lock().unwrap();

    state.log.line("=== LATENCY SAMPLES (edit-file-write -> new-marker-observed-over-channel) ===");
    let mut latencies_ms: Vec<f64> = Vec::new();
    let mut heap_survived = true;
    for s in samples.iter() {
        let ms = s.latency.as_secs_f64() * 1000.0;
        state.log.line(&format!(
            "iter {:>2} marker={:<10} recompile_ok={:<5} observed={:<5} latency={:>8.1}ms tap_count={:?} (baseline={baseline_tap})",
            s.iteration, s.marker, s.recompile_ok, s.observed, ms, s.observed_tap_count
        ));
        if s.observed {
            latencies_ms.push(ms);
        }
        if let Some(tc) = s.observed_tap_count {
            if tc != baseline_tap {
                heap_survived = false;
            }
        } else {
            heap_survived = false;
        }
        if !s.recompile_diagnostics.is_empty() {
            state.log.line(&format!("  iter {} recompile diagnostics: {:?}", s.iteration, s.recompile_diagnostics));
        }
        if let Some(r) = &s.reload_response {
            state.log.line(&format!("  iter {} reloadSources response: {r}", s.iteration));
        }
        if let Some(r) = &s.reassemble_response {
            state.log.line(&format!("  iter {} ext.flutter.reassemble response: {r}", s.iteration));
        }
    }

    latencies_ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    if latencies_ms.is_empty() {
        state.log.line("=== NO SUCCESSFUL RELOADS OBSERVED - cannot compute latency stats ===");
    } else {
        let min = latencies_ms.first().unwrap();
        let max = latencies_ms.last().unwrap();
        let median = latencies_ms[latencies_ms.len() / 2];
        let mean: f64 = latencies_ms.iter().sum::<f64>() / latencies_ms.len() as f64;
        state.log.line(&format!(
            "=== LATENCY SUMMARY: n={} min={:.1}ms median={:.1}ms max={:.1}ms mean={:.1}ms ===",
            latencies_ms.len(), min, median, max, mean
        ));
        state.log.line(&format!(
            "=== 500ms parity threshold: {} (max sample was {:.1}ms) ===",
            if *max < 500.0 { "MET" } else { "NOT MET" },
            max
        ));
    }

    state.log.line(&format!(
        "=== DART HEAP STATE: baseline tapCount={baseline_tap}, survived all reloads unchanged = {heap_survived} \
         (if true: this is genuine HOT RELOAD; if false: state was lost, this is HOT RESTART behavior) ==="
    ));

    state.log.line(&format!("final RSS: {} kB", rss_kb()));

    if let Some(engine) = state.engine.take() {
        unsafe {
            let _: () = objc2::msg_send![&engine, shutDownEngine];
        }
        state.log.line("shutDownEngine sent");
    }

    state.finishing = true;
}
