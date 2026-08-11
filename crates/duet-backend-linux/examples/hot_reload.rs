//! The hot-reload proof: a real Flutter engine, a real edit to a real `.dart`
//! file, a real `reloadSources` — and the Duet store's contents still there
//! afterwards.
//!
//! This is the claim Duet's governing principle makes about reload. "State
//! survives teardown. Events don't" — and a reload is a far weaker event than
//! a teardown, so if state does not survive one, nothing else in the framework
//! is trustworthy. Spike C proved the Dart heap survives
//! (`spikes/spike-c-macos/FINDINGS.md`); this proves the **shared store**
//! does, over the real `duet/rpc` transport, through the real
//! [`duet_dev::ReloadDriver`].
//!
//! # Build and run
//!
//! ```text
//! (cd fixtures/duet_guest && flutter build linux --debug)
//! FLUTTER_LINUX_RENDERER=software GDK_BACKEND=x11 \
//!   cargo run -p duet-backend-linux --example hot_reload
//! ```
//!
//! `FLUTTER_LINUX_RENDERER=software` is required under WSLg — no usable GL
//! reaches the embedder there ("Could not determine GL version") — while
//! GL-capable desktops may omit it; the crate disables Impeller either way
//! (L-F4). `DUET_FLUTTER_BUNDLE` overrides the bundle's `data` directory
//! (default `fixtures/duet_guest/build/linux/x64/debug/bundle/data`).
//! `FLUTTER_ROOT` selects the SDK; when unset it is discovered from
//! `which flutter` (the first hit, resolved through any symlink, is
//! `<root>/bin/flutter`). `DUET_HOT_RELOAD_ITERATIONS` sets how many reloads
//! to measure (default 10, matching Spike C's canonical run).
//!
//! # The five claims
//!
//! 1. **The reload was applied.** `reloadSources` reported `success: true`, and
//!    reported far more libraries *kept* than *received* — which is what
//!    distinguishes an incremental reload from a full one.
//! 2. **The Dart-side change took effect.** The marker this driver wrote into
//!    the source file came back out of the store, having been rendered.
//! 3. **The Duet store survived.** A value Rust wrote *before* the first
//!    reload is still exactly there after the last one.
//! 4. **It was a reload, not a restart.** The guest's `State` nonce — assigned
//!    in `initState`, which a reload never re-runs — is unchanged, and its
//!    frame counter only ever climbed.
//! 5. **Latency.** Reported below, with a precise statement of what it spans.
//!
//! # What the latency number actually measures
//!
//! The clock starts at this process's `std::fs::write` of the Dart source and
//! stops when the **new marker value is readable from the Rust store**. In
//! between:
//!
//! ```text
//! fs::write -> frontend_server recompile -> reloadSources -> ext.flutter.reassemble
//!   -> Flutter rebuilds the tree -> build() puts the new marker in the frame
//!   -> the frame is produced -> addPostFrameCallback fires
//!   -> the guest writes it over duet/rpc -> duet-protocol decodes it
//!   -> the runtime's core thread applies it -> this thread reads it back
//! ```
//!
//! **This is not a camera.** Unlike the macOS reference machine (whose runs
//! had no reachable on-screen WindowServer for spawned processes), this
//! machine has a display — WSLg's virtual one — and the window genuinely maps
//! onto it; but the clock still stops at Flutter's post-frame callback, not
//! at the monitor: "rendered" means Flutter produced the frame in-process,
//! which is the same thing Spike C measured and the same content this
//! example's own PNG is read back from (the X server's copy of the window).
//! Nothing here observes a monitor and nothing here claims to. (What an
//! autonomous run on this machine still cannot verify is real human input —
//! nobody is at the keyboard — but this example needs none.)
//!
//! It is also **strictly more work than Spike C measured**. Spike C stopped
//! when a bespoke platform-channel handler saw the marker; this continues
//! through `duet-protocol`'s decode and the runtime's core thread into the
//! store.
//!
//! On the macOS reference machine this example measured *faster* than Spike
//! C's 123.3 ms median — around 40 ms. That is not the machinery getting
//! quicker: Spike C's fixture rebuilt a whole `MaterialApp` with a `Ticker`
//! running, and this one rebuilds three widgets, so the rebuild-and-paint leg
//! is far cheaper. The recompile leg, which is the part this crate actually
//! controls, is comparable in both (6-19 ms there against Spike C's 8.8-21.8
//! ms). Read whatever number this machine prints the same way: as "the driver
//! adds no measurable overhead to Spike C's recipe", not as a speedup.
//!
//! # Structure: a driver thread, and a main thread that keeps pumping
//!
//! Spike C's threading arrangement, for its reasons. The reload driver blocks
//! on a child process's pipes and a TCP socket; the Flutter engine's platform
//! work runs on *this* thread — the GTK embedder schedules everything onto
//! the default GLib main context, which is also what `tao`'s loop iterates
//! (the coexistence `spikes/spike-b-linux/FINDINGS.md` proved over 180 s,
//! 722/722 closed-loop events). So all blocking I/O happens on a background
//! thread that touches no GTK or engine API at all — it holds only a
//! [`StoreHandle`], which is `Send + Sync` — and the main thread keeps
//! turning the run loop, which is what lets Flutter actually apply the
//! reload when it arrives.
//!
//! # It edits a tracked file, and puts it back
//!
//! `fixtures/duet_guest/lib/reload_driver.dart` is in git. This rewrites one
//! string literal in it, repeatedly, and restores `MARKER_V1` on every exit
//! path — including a panic in the driver thread, which is why the restore
//! also runs from the main thread at the end.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant as StdInstant};

use duet_backend_linux::{DuetEvent, FlutterSurface, LinuxBackend, ProxySink};
use duet_core::{Path as DuetPath, SubscriberId, Value};
use duet_dev::{
    CompilerConfig, DriverConfig, FlutterSdk, PackageConfig, Reload, ReloadDriver, Timeouts,
    VmServiceUrl,
};
use duet_host::WindowBackend;
use duet_runtime::{Runtime, StoreHandle};
use duet_supervisor::SurfaceId;

use gtk::prelude::*;
use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::platform::unix::WindowExtUnix;

/// How long the whole run gets before it is declared hung.
///
/// Generous: a cold `frontend_server` baseline compile of this fixture takes
/// seconds (Spike C measured 3.8 s for a smaller app), and ten reloads follow.
const DEADLINE: Duration = Duration::from_secs(180);

/// Milliseconds of run loop per turn.
///
/// The macOS source calls this `TURN_SECS`, which is a misnomer — the value
/// has always been fed to `Duration::from_millis`. Same value, honest name
/// (as the Windows port first noted).
const TURN_MILLIS: u64 = 25;

/// Where the guest publishes what it rendered. Must match
/// `kReloadReportPath` in `fixtures/duet_guest/lib/reload_driver.dart`.
const REPORT_PATH: &str = "reload";

/// The path whose value must survive every reload.
const WITNESS_PATH: &str = "hostWitness";

/// The value written there before the first reload, and required afterwards.
///
/// A specific number rather than a bool: "the value is still 4242" is a
/// stronger statement than "something is still there", and it would catch a
/// store that was reseeded from its initial contents.
const WITNESS: i64 = 4242;

/// Where the host tells the guest which driver to run. Must match `kModePath`.
const MODE_PATH: &str = "mode";

/// The mode that selects the reload driver. Must match `kReloadMode`.
const MODE: &str = "reload";

/// The Dart source this driver edits.
const SOURCE: &str = "fixtures/duet_guest/lib/reload_driver.dart";

/// The declaration whose string literal is rewritten.
const MARKER_DECLARATION: &str = "const String kReloadMarker = '";

/// The value the file is left at.
const BASELINE_MARKER: &str = "MARKER_V1";

/// The Dart entrypoint, as a `package:` URI.
const ENTRYPOINT: &str = "package:duet_guest/main.dart";

/// The Flutter project.
const PROJECT: &str = "fixtures/duet_guest";

/// One measured reload.
#[derive(Debug, Clone)]
struct Sample {
    marker: String,
    /// `fs::write` to the new marker being readable from the Rust store.
    latency: Option<Duration>,
    /// What the driver reported.
    outcome: String,
    applied: bool,
    received_libraries: Option<i64>,
    saved_libraries: Option<i64>,
    /// The guest's `State` nonce at that point.
    nonce: Option<i64>,
    /// The guest's frame counter at that point.
    frames: Option<i64>,
}

/// What the driver thread hands back to the main thread.
#[derive(Default)]
struct Results {
    samples: Mutex<Vec<Sample>>,
    /// A setup failure, if the driver could not even start.
    setup_failure: Mutex<Option<String>>,
    done: AtomicBool,
}

fn flutter_data_dir() -> String {
    std::env::var("DUET_FLUTTER_BUNDLE")
        .unwrap_or_else(|_| "fixtures/duet_guest/build/linux/x64/debug/bundle/data".to_string())
}

/// The Flutter SDK root: `FLUTTER_ROOT` when set, otherwise discovered from
/// `which flutter` — the first hit is `<root>/bin/flutter`, so the SDK root
/// is its parent's parent. (The macOS source hard-codes that machine's
/// checkout; this machine's has to be found.) The hit is resolved through
/// symlinks first: a Linux PATH commonly reaches `flutter` through one, and
/// the SDK's artefacts live beside the real binary, not the link.
fn flutter_root() -> Result<String, String> {
    if let Ok(root) = std::env::var("FLUTTER_ROOT") {
        return Ok(root);
    }
    let output = std::process::Command::new("which")
        .arg("flutter")
        .output()
        .map_err(|e| format!("running `which flutter`: {e}"))?;
    if !output.status.success() {
        return Err("`which flutter` found nothing on PATH — set FLUTTER_ROOT".to_string());
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let first = stdout
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .ok_or_else(|| "`which flutter` produced no output — set FLUTTER_ROOT".to_string())?;
    let real = std::fs::canonicalize(first).unwrap_or_else(|_| Path::new(first).to_path_buf());
    let root = real
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| format!("cannot derive FLUTTER_ROOT from {first:?}"))?;
    Ok(root.display().to_string())
}

fn iterations() -> usize {
    std::env::var("DUET_HOT_RELOAD_ITERATIONS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(10)
}

/// The store both sides share.
///
/// Every key the guest writes must be seeded: `set` never creates intermediate
/// nodes, and a top-level key it has never seen is still a key it can insert —
/// but seeding them keeps the shape explicit.
fn initial_root() -> Value {
    Value::map([
        (MODE_PATH, Value::Str(MODE.to_string())),
        (REPORT_PATH, Value::Null),
        (WITNESS_PATH, Value::Int(0)),
    ])
}

fn main() {
    let data_dir = flutter_data_dir();
    println!("[hot_reload] Flutter data dir: {data_dir}");

    // Engine switches, set before anything boots. This is what retires Spike
    // C's fd-1 redirect for an in-process host: the VM service is *told* its
    // port, so its URI is known without observing any output at all. See
    // `duet_dev::locate` for the measurement behind this (made against the
    // macOS embedder — the GTK embedder shares the engine's common
    // environment-switch reader, and this run is what verifies it here) and
    // for why the child-process route is the default everywhere a child
    // exists.
    let port = match duet_dev::free_port() {
        Ok(port) => port,
        Err(e) => {
            println!("FAIL: setup - could not reserve a port for the VM service: {e}");
            std::process::exit(1);
        }
    };
    // No delay-load contortions here, unlike the Windows port: the engine
    // reads switches with `std::getenv` (`engine_switches.cc`,
    // `GetSwitchesFromEnvironment`) when it starts, `libflutter_linux_gtk.so`
    // shares glibc's one `environ` with this code, and the engine does not
    // start until the boot inside `App::open_window` below — so a plain
    // `set_var` first is all the ordering there is, exactly as on macOS.
    //
    // SAFETY: this is the first statement of `main` that touches the
    // environment, no other thread exists yet, and the engine reads these
    // during the boot in `App::open_window` below — on this same thread.
    unsafe {
        for (name, value) in duet_dev::engine_switches(port) {
            std::env::set_var(name, value);
        }
    }
    let vm_service = VmServiceUrl::loopback(port);
    println!(
        "[hot_reload] VM service will be at {}",
        vm_service.websocket()
    );

    let event_loop = EventLoopBuilder::<DuetEvent>::with_user_event().build();
    // The metronome. tao's GTK loop parks a pending `WaitUntil` inside a
    // fully blocking `gtk_main_iteration` with no timer source armed for the
    // deadline, so on a quiet session a 50 ms turn can stall for ~10 s until
    // some unrelated X11 event wakes the context — measured under WSLg
    // before this thread existed. A proxy send is a glib channel source and
    // *does* wake the context, so a thread posting `DuetEvent::Tick` at turn
    // cadence keeps the deadline checks honest; the Tick events themselves
    // are no-ops. (The reload latency this example measures is unaffected:
    // its clock starts at `fs::write` and ends at a store readback, neither
    // of which waits on a `WaitUntil` deadline.)
    let metronome = event_loop.create_proxy();
    std::thread::spawn(move || {
        while metronome.send_event(DuetEvent::Tick).is_ok() {
            std::thread::sleep(Duration::from_millis(50));
        }
    });
    let proxy = event_loop.create_proxy();
    let runtime = Runtime::spawn(initial_root(), ProxySink::new(proxy));

    let mut state = match setup(&data_dir, runtime, vm_service) {
        Ok(state) => state,
        Err(reason) => {
            println!("FAIL: setup - {reason}");
            std::process::exit(1);
        }
    };

    event_loop.run(move |event, target, control_flow| match event {
        Event::NewEvents(StartCause::Init) => {
            if let Err(reason) = state.open_window(target) {
                println!("FAIL: setup - {reason}");
                finish(&mut state, 1);
            }
            *control_flow = ControlFlow::WaitUntil(std::time::Instant::now());
        }
        Event::NewEvents(StartCause::ResumeTimeReached { .. }) => {
            advance(&mut state);
            *control_flow = if state.finished {
                ControlFlow::Exit
            } else {
                ControlFlow::WaitUntil(
                    std::time::Instant::now() + Duration::from_millis(TURN_MILLIS),
                )
            };
        }
        // Notifications are not what this example asserts on — it reads the
        // store directly — but draining them keeps the sink's queue from
        // growing across a long run.
        Event::UserEvent(_) => {}
        _ => {}
    });
}

/// Everything the driver closure needs across turns.
struct App {
    start: StdInstant,
    step: Step,
    backend: LinuxBackend,
    surface_id: SurfaceId,
    /// The subscriber the surface will be built as, reserved in `setup` —
    /// on Linux the surface itself only exists once the window does.
    subscriber: SubscriberId,
    /// Dropped before the engine — see `FlutterSurface`'s `Drop`.
    surface: Option<FlutterSurface>,
    runtime: Option<Runtime>,
    store: StoreHandle,
    report: DuetPath,
    witness: DuetPath,
    vm_service: VmServiceUrl,
    results: Arc<Results>,
    driver_started: bool,
    /// The guest's first report, taken before any reload.
    baseline: Option<GuestReport>,
    finished: bool,
    timed_out: bool,
}

/// What the guest published, decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
struct GuestReport {
    marker: String,
    frames: i64,
    nonce: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    /// Wait for the guest to render and publish once.
    AwaitGuest,
    /// Write the witness value, then start the driver thread.
    Arm,
    /// Wait for the driver thread to finish its iterations.
    AwaitReloads,
    /// Assert everything and print.
    Report,
}

/// Parses paths and creates the backend. No engine yet — where the other two
/// platforms boot here and open the window later, Linux boot realizes the
/// Flutter view *into* the surface's window (L-F1), so the window must exist
/// first, and creating one needs the event loop's `target`.
fn setup(data_dir: &str, runtime: Runtime, vm_service: VmServiceUrl) -> Result<App, String> {
    let subscriber = runtime.next_subscriber_id();
    let report = DuetPath::parse(REPORT_PATH).map_err(|e| format!("{REPORT_PATH:?}: {e:?}"))?;
    let witness = DuetPath::parse(WITNESS_PATH).map_err(|e| format!("{WITNESS_PATH:?}: {e:?}"))?;

    let surface_id = SurfaceId::from_raw(1);
    let backend = LinuxBackend::new(data_dir);

    Ok(App {
        start: StdInstant::now(),
        step: Step::AwaitGuest,
        backend,
        surface_id,
        subscriber,
        surface: None,
        store: runtime.handle(),
        runtime: Some(runtime),
        report,
        witness,
        vm_service,
        results: Arc::new(Results::default()),
        driver_started: false,
        baseline: None,
        finished: false,
        timed_out: false,
    })
}

impl App {
    /// Opens the window, boots the engine into it, and attaches the view.
    ///
    /// The order is the load-bearing Linux difference: `open_window` MUST
    /// precede `start_renderer`, because boot realizes the implicit view into
    /// the surface's still-invisible window (L-F1/L-F2) — a renderer started
    /// with no window open boots headless into a private holding window and
    /// can never be attached afterwards, since the realized view can never
    /// move (L-F3). The other two backends boot in `setup` and open here; on
    /// Linux the two steps swap, and everything downstream is unchanged.
    ///
    /// A real view, unlike `examples/flutter_state.rs`, because this example's
    /// claim is about a **rendered frame**. Without one, `reassemble` would
    /// rebuild a tree that composites into nothing, and the post-frame
    /// callback the measurement depends on would never fire.
    fn open_window(
        &mut self,
        target: &tao::event_loop::EventLoopWindowTarget<DuetEvent>,
    ) -> Result<(), String> {
        self.backend
            .open_window(self.surface_id, target, "Duet hot reload")
            .map_err(|e| format!("opening the window failed: {e}"))?;
        self.backend
            .start_renderer(self.surface_id)
            .map_err(|e| format!("could not boot a Flutter engine: {e}"))?;
        let engine = self
            .backend
            .engine(self.surface_id)
            .ok_or_else(|| "the backend booted a renderer but has no engine".to_string())?;
        let surface = FlutterSurface::new(engine, self.store.clone(), self.subscriber)
            .map_err(|e| format!("could not register the duet/rpc handler: {e}"))?;
        self.surface = Some(surface);
        println!("[hot_reload] engine booted; duet/rpc registered");
        self.backend
            .attach_view(self.surface_id)
            .map_err(|e| format!("attaching the Flutter view failed: {e}"))?;
        println!("[hot_reload] window open, Flutter view attached");
        Ok(())
    }
}

/// Advances at most one stage per run-loop turn.
fn advance(state: &mut App) {
    if state.start.elapsed() > DEADLINE && state.step != Step::Report {
        state.timed_out = true;
        state.step = Step::Report;
    }
    match state.step {
        Step::AwaitGuest => await_guest(state),
        Step::Arm => arm(state),
        Step::AwaitReloads => await_reloads(state),
        Step::Report => report(state),
    }
}

/// Reads the guest's published report out of the shared store.
fn read_report(store: &StoreHandle, path: &DuetPath) -> Option<GuestReport> {
    let Ok(Some(Value::Map(entries))) = store.get(path) else {
        return None;
    };
    let text = |key: &str| match entries.get(key) {
        Some(Value::Str(s)) => Some(s.clone()),
        _ => None,
    };
    let number = |key: &str| match entries.get(key) {
        Some(Value::Int(n)) => Some(*n),
        _ => None,
    };
    Some(GuestReport {
        marker: text("marker")?,
        frames: number("frames")?,
        nonce: number("nonce")?,
    })
}

fn await_guest(state: &mut App) {
    let Some(report) = read_report(&state.store, &state.report) else {
        return;
    };
    println!(
        "[hot_reload] guest rendered and published after {:.2}s: {report:?}",
        state.start.elapsed().as_secs_f64()
    );
    state.baseline = Some(report);
    state.step = Step::Arm;
}

/// Writes the witness, then starts the reload driver on its own thread.
fn arm(state: &mut App) {
    if state.driver_started {
        state.step = Step::AwaitReloads;
        return;
    }
    if let Err(e) = state.store.set(&state.witness, Value::Int(WITNESS)) {
        println!("FAIL: setup - writing the witness value failed: {e}");
        state.step = Step::Report;
        return;
    }
    println!("[hot_reload] Rust wrote {WITNESS_PATH} = Int({WITNESS}) BEFORE any reload");

    let store = state.store.clone();
    let results = Arc::clone(&state.results);
    let report_path = state.report.clone();
    let vm_service = state.vm_service.clone();
    let started = std::thread::Builder::new()
        .name("duet-hot-reload-driver".to_string())
        .spawn(move || drive(&store, &report_path, vm_service, &results));
    match started {
        Ok(_) => {
            state.driver_started = true;
            state.step = Step::AwaitReloads;
        }
        Err(e) => {
            println!("FAIL: setup - could not start the reload driver thread: {e}");
            state.step = Step::Report;
        }
    }
}

fn await_reloads(state: &mut App) {
    if state.results.done.load(Ordering::SeqCst) {
        state.step = Step::Report;
    }
}

/// The reload driver, on its own thread. Touches no GTK or engine API.
fn drive(store: &StoreHandle, report_path: &DuetPath, vm_service: VmServiceUrl, results: &Results) {
    let outcome = run_reloads(store, report_path, vm_service, results);
    if let Err(reason) = outcome {
        if let Ok(mut slot) = results.setup_failure.lock() {
            *slot = Some(reason);
        }
    }
    // Always put the tracked fixture back, whatever happened.
    if let Err(e) = write_marker(BASELINE_MARKER) {
        println!("[hot_reload] WARNING could not restore {SOURCE}: {e}");
    }
    results.done.store(true, Ordering::SeqCst);
}

/// Resolves the SDK artefacts for this machine.
///
/// [`FlutterSdk::locate`] resolves the platform's own executable name
/// (`dartaotruntime` here) and names whichever artefact is missing.
fn locate_sdk() -> Result<FlutterSdk, String> {
    let root = flutter_root()?;
    FlutterSdk::locate(&root).map_err(|e| e.to_string())
}

/// Starts a [`ReloadDriver`] and runs the measured iterations.
fn run_reloads(
    store: &StoreHandle,
    report_path: &DuetPath,
    vm_service: VmServiceUrl,
    results: &Results,
) -> Result<(), String> {
    let sdk = locate_sdk()?;
    let package_config_path = duet_dev::package_config(PROJECT).map_err(|e| e.to_string())?;
    let packages = PackageConfig::read(&package_config_path).map_err(|e| e.to_string())?;
    // Not `canonicalize`, which the macOS source uses: `PackageConfig`
    // resolves its package roots lexically, symlinks unresolved, so a
    // checkout reached through one — routine in Linux home-directory
    // layouts — would stop matching a canonicalized source path and silently
    // degrade the invalidated URI to `file://`, the exact failure
    // `duet_dev::packages` exists to prevent. `std::path::absolute` is
    // lexical, matching how the package roots themselves were resolved —
    // the same reasoning the Windows port arrived at for its `\\?\` paths.
    let source = std::path::absolute(SOURCE).map_err(|e| format!("{SOURCE}: {e}"))?;
    let invalidated = vec![packages.uri_for(&source)];
    println!("[hot_reload] invalidating {invalidated:?} on each recompile");

    let started = ReloadDriver::start(DriverConfig {
        compiler: CompilerConfig {
            sdk,
            package_config: package_config_path,
            output_dill: std::env::temp_dir()
                .join(format!("duet-hot-reload-{}", std::process::id()))
                .join("app.dill"),
        },
        entrypoint: ENTRYPOINT.to_string(),
        vm_service,
        timeouts: Timeouts::default(),
    })
    .map_err(|e| e.to_string())?;
    if !started.baseline.ok {
        return Err(format!(
            "the fixture does not compile: {:?}",
            started.baseline.diagnostics
        ));
    }
    println!(
        "[hot_reload] baseline compile in {:?}; isolate {:?}",
        started.baseline.elapsed,
        started.driver.isolate()
    );
    let mut driver = started.driver;

    for iteration in 0..iterations() {
        // V1 is the value already running, so the first new one is V2.
        let marker = format!("MARKER_V{}", iteration + 2);

        // The clock starts here, at the source edit — not at the RPC.
        let t0 = StdInstant::now();
        write_marker(&marker).map_err(|e| format!("editing {SOURCE}: {e}"))?;

        let outcome = driver.reload(&invalidated).map_err(|e| e.to_string())?;
        let (applied, received, saved, description) = match &outcome {
            Reload::Applied { report, timings } => (
                true,
                report.received_libraries,
                report.saved_libraries,
                format!(
                    "applied (recompile {:?}, reload {:?}, reassemble {:?})",
                    timings.recompile, timings.reload, timings.reassemble
                ),
            ),
            Reload::Declined { report, .. } => {
                (false, None, None, format!("declined: {:?}", report.notices))
            }
            Reload::CompileFailed { diagnostics, .. } => (
                false,
                None,
                None,
                format!("compile failed: {diagnostics:?}"),
            ),
        };

        // Stop the clock when the NEW marker is readable from the Rust store.
        let observed = await_marker(store, report_path, &marker, Duration::from_secs(10));
        let latency = observed.as_ref().map(|_| t0.elapsed());

        println!(
            "[hot_reload] iter {iteration}: {marker} {description}; observed={} latency={latency:?}",
            observed.is_some()
        );
        if let Ok(mut samples) = results.samples.lock() {
            samples.push(Sample {
                marker,
                latency,
                outcome: description,
                applied,
                received_libraries: received,
                saved_libraries: saved,
                nonce: observed.as_ref().map(|r| r.nonce),
                frames: observed.as_ref().map(|r| r.frames),
            });
        }
    }

    driver.shutdown();
    Ok(())
}

/// Polls the store until the guest reports `marker`, or gives up.
///
/// Polling rather than subscribing on purpose: this thread must not hold a
/// notification sink, because the sink for this process marshals onto the
/// `tao` event loop that the main thread owns.
fn await_marker(
    store: &StoreHandle,
    path: &DuetPath,
    marker: &str,
    budget: Duration,
) -> Option<GuestReport> {
    let deadline = StdInstant::now() + budget;
    while StdInstant::now() < deadline {
        if let Some(report) = read_report(store, path) {
            if report.marker == marker {
                return Some(report);
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    None
}

/// Rewrites the one string literal in the tracked Dart source.
///
/// Deterministic regardless of the value currently there, so the caller does
/// not have to track it — and so a run interrupted half way still restores
/// cleanly on the next one.
fn write_marker(marker: &str) -> Result<(), String> {
    let content = std::fs::read_to_string(SOURCE).map_err(|e| e.to_string())?;
    let Some(start) = content.find(MARKER_DECLARATION) else {
        return Err(format!(
            "{SOURCE} no longer contains {MARKER_DECLARATION:?}; \
             a reformat would silently break this proof, so it stops instead"
        ));
    };
    let value_start = start + MARKER_DECLARATION.len();
    let Some(length) = content[value_start..].find('\'') else {
        return Err(format!("{SOURCE} has an unterminated marker literal"));
    };
    let mut updated = String::with_capacity(content.len());
    updated.push_str(&content[..value_start]);
    updated.push_str(marker);
    updated.push_str(&content[value_start + length..]);
    std::fs::write(SOURCE, updated).map_err(|e| e.to_string())
}

/// Prints every claim, rasterizes evidence, tears down, and exits.
fn report(state: &mut App) {
    if state.finished {
        return;
    }
    let samples: Vec<Sample> = state
        .results
        .samples
        .lock()
        .map(|s| s.clone())
        .unwrap_or_default();
    let setup_failure = state
        .results
        .setup_failure
        .lock()
        .ok()
        .and_then(|s| s.clone());
    let baseline = state.baseline.clone();
    let final_report = read_report(&state.store, &state.report);
    let witness = state.store.get(&state.witness);

    // Rasterize what is on screen now — the frame carrying the last marker.
    // Read back from the X server's copy of the window, not a monitor grab;
    // see the module docs.
    if let Some(window) = state.backend.window(state.surface_id) {
        // Into the temp directory, not the repository — the same choice
        // `examples/lifecycle.rs` makes. This is evidence for whoever runs it,
        // not an artefact to commit.
        let png = std::env::temp_dir().join("duet_hot_reload_after.png");
        match rasterize(window, &png.display().to_string()) {
            Ok(bytes) => println!("[hot_reload] wrote {} ({bytes} bytes)", png.display()),
            Err(e) => println!("[hot_reload] rasterization failed: {e}"),
        }
    }

    println!();
    println!("=== Duet hot-reload report ===");
    if state.timed_out {
        println!("TIMED OUT after {DEADLINE:?} at stage {:?}", state.step);
    }
    if let Some(reason) = &setup_failure {
        println!("driver setup failed: {reason}");
    }
    println!("baseline guest report:  {baseline:?}");
    println!("final guest report:     {final_report:?}");
    println!();

    let mut failures = 0;
    let mut check = |label: &str, ok: bool, detail: String| {
        println!("{}: {label} - {detail}", if ok { "PASS" } else { "FAIL" });
        if !ok {
            failures += 1;
        }
    };

    // 1. Every reload was applied incrementally.
    let applied = samples.iter().filter(|s| s.applied).count();
    check(
        "every reload was applied",
        !samples.is_empty() && applied == samples.len(),
        format!(
            "{applied}/{} reload(s) reported success{}",
            samples.len(),
            samples
                .iter()
                .find(|s| !s.applied)
                .map(|s| format!("; first failure: {}", s.outcome))
                .unwrap_or_default()
        ),
    );

    // The counts are what separate an incremental reload from a full one, and
    // a full one would lose exactly the heap state this proof is about.
    let incremental = samples
        .iter()
        .all(|s| match (s.received_libraries, s.saved_libraries) {
            (Some(received), Some(saved)) => saved > received,
            _ => false,
        });
    check(
        "each reload was incremental, not a full reload",
        !samples.is_empty() && incremental,
        format!(
            "libraries received vs kept: {:?}",
            samples
                .iter()
                .map(|s| (s.received_libraries, s.saved_libraries))
                .collect::<Vec<_>>()
        ),
    );

    // 2. The Dart change took effect, and was rendered.
    let observed = samples.iter().filter(|s| s.latency.is_some()).count();
    check(
        "the Dart-side change took effect in a rendered frame",
        !samples.is_empty() && observed == samples.len(),
        format!(
            "{observed}/{} new marker(s) came back out of the store after being built \
             into a frame; last was {:?}",
            samples.len(),
            final_report.as_ref().map(|r| r.marker.clone())
        ),
    );

    // 3. THE HEADLINE: the Duet store survived.
    let survived = matches!(&witness, Ok(Some(Value::Int(n))) if *n == WITNESS);
    check(
        "the Duet store's contents survived every reload",
        survived && !samples.is_empty(),
        format!(
            "{WITNESS_PATH} was written as Int({WITNESS}) before the first reload and reads \
             back as {witness:?} after {} reload(s)",
            samples.len()
        ),
    );

    // 4. Reload, not restart.
    let nonces: Vec<i64> = samples.iter().filter_map(|s| s.nonce).collect();
    let baseline_nonce = baseline.as_ref().map(|r| r.nonce);
    let same_isolate = baseline_nonce.is_some_and(|first| {
        !nonces.is_empty()
            && nonces.iter().all(|n| Some(*n) == baseline_nonce)
            && first == nonces[0]
    });
    check(
        "it was a hot reload, not a restart",
        same_isolate,
        format!(
            "the guest's initState-assigned nonce was {baseline_nonce:?} before any reload \
             and {:?} after each one (a restart would produce a different one)",
            nonces
        ),
    );

    let frames: Vec<i64> = samples.iter().filter_map(|s| s.frames).collect();
    let climbing = frames.windows(2).all(|w| w[1] >= w[0])
        && baseline
            .as_ref()
            .is_some_and(|b| frames.first().is_none_or(|f| *f >= b.frames));
    check(
        "the guest's own frame counter never reset",
        climbing && !frames.is_empty(),
        format!(
            "frames after each reload: {frames:?} (baseline {:?})",
            baseline.as_ref().map(|b| b.frames)
        ),
    );

    // 5. Latency.
    let mut latencies: Vec<f64> = samples
        .iter()
        .filter_map(|s| s.latency)
        .map(|d| d.as_secs_f64() * 1000.0)
        .collect();
    latencies.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    println!();
    if latencies.is_empty() {
        println!("no latency samples: no reload was ever observed to take effect");
        failures += 1;
    } else {
        let min = latencies.first().copied().unwrap_or(0.0);
        let max = latencies.last().copied().unwrap_or(0.0);
        let median = latencies[latencies.len() / 2];
        let mean = latencies.iter().sum::<f64>() / latencies.len() as f64;
        println!(
            "LATENCY (fs::write -> the new marker readable from the Rust store, having been \
             built into a rendered frame):"
        );
        println!(
            "  n={} min={min:.1}ms median={median:.1}ms max={max:.1}ms mean={mean:.1}ms",
            latencies.len()
        );
        println!("  per-iteration, in the order they ran:");
        for sample in &samples {
            match sample.latency {
                Some(latency) => println!(
                    "    {:<12} {:>7.1} ms",
                    sample.marker,
                    latency.as_secs_f64() * 1000.0
                ),
                None => println!("    {:<12}   never observed", sample.marker),
            }
        }
        println!(
            "  the 500 ms parity bar: {} (max sample {max:.1}ms)",
            if max < 500.0 { "MET" } else { "NOT MET" }
        );
        println!(
            "  Spike C measured 123.3 ms median (n=10) on macOS for the same edit-to-frame \
             path, stopping at a bespoke platform-channel handler; this number goes further, \
             through duet-protocol's decode and the runtime's core thread — and on the macOS \
             reference machine still came out smaller (~40 ms). The difference is the widget \
             tree, not the machinery: Spike C's fixture rebuilt a whole MaterialApp with a \
             running Ticker, and this one rebuilds three widgets. (Recompile time there was \
             comparable in both: 6-19 ms against Spike C's 8.8-21.8 ms.)"
        );
    }
    println!();

    finish(state, u8::from(failures > 0));
}

/// Tears down in the required order and exits.
fn finish(state: &mut App, code: u8) {
    state.finished = true;
    // The surface unregisters its channel handler before the engine shuts
    // down — the messenger holds the handler's `user_data` pointer past
    // registration, and `destroy_renderer` *forces* the engine's dispose
    // (see the crate's engine docs), after which there is no live channel
    // table to unregister from.
    drop(state.surface.take());
    if let Err(e) = state.backend.destroy_renderer(state.surface_id) {
        println!("[hot_reload] destroying the renderer failed: {e}");
    }
    // After `destroy_renderer` no `FlView` lives in the window, so this close
    // drops it on the spot and returns true (it logically closed an open
    // window). The reverse order would also be fine — on Linux a close under
    // a live renderer merely defers the physical drop to `destroy_renderer` —
    // but this order keeps the three backends' examples identical.
    let _ = state.backend.close_window(state.surface_id);
    if let Some(runtime) = state.runtime.take() {
        if let Err(e) = runtime.shutdown() {
            println!("[hot_reload] runtime shutdown failed: {e}");
        }
    }
    // Belt and braces: the driver thread restores this too, but it may have
    // panicked, and leaving a tracked fixture edited is not acceptable.
    if let Err(e) = write_marker(BASELINE_MARKER) {
        println!("[hot_reload] WARNING could not restore {SOURCE}: {e}");
    }

    if code == 0 {
        println!(
            "ALL PASS: a real Flutter engine hot-reloaded a real edit, and the Duet store \
             kept its contents"
        );
    } else {
        println!("{code} check group(s) FAILED");
    }
    std::process::exit(i32::from(code));
}

/// Rasterizes the window holding the Flutter view to a PNG.
///
/// `gdk_pixbuf_get_from_window` reads the window's composited contents back
/// from the X server — under WSLg, the same server-side copy the virtual
/// display shows — so nothing here depends on desktop occlusion, and no
/// `unsafe` is involved: the whole capture is safe gtk-rs API. The Linux
/// spike had no snapshot helper to port (the other two examples inherited
/// theirs); this is the plain readback. The whole `tao` window is captured
/// rather than the `FlView` alone — the view fills the window's vbox, so the
/// difference is at most the window frame.
fn rasterize(window: &tao::window::Window, path: &str) -> Result<usize, String> {
    let gdk_window = window
        .gtk_window()
        .window()
        .ok_or_else(|| "the tao window has no GdkWindow - was it ever realized?".to_string())?;
    let (w, h) = (gdk_window.width(), gdk_window.height());
    if w <= 0 || h <= 0 {
        return Err("the window has an empty extent".to_string());
    }
    let pixbuf = gdk_window
        .pixbuf(0, 0, w, h)
        .ok_or_else(|| "gdk_pixbuf_get_from_window returned nothing".to_string())?;
    pixbuf
        .savev(path, "png", &[])
        .map_err(|e| format!("writing {path}: {e}"))?;
    std::fs::metadata(path)
        .map(|m| m.len() as usize)
        .map_err(|e| format!("statting {path}: {e}"))
}
