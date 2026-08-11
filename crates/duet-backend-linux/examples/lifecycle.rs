//! The RSS proof: drives one real Flutter surface through a full lifecycle —
//! window open, engine boot, view attach, suspend, teardown — over the real
//! `duet-host` orchestration, and measures this process's resident set size
//! at every stage.
//!
//! This is the Linux re-measurement of Duet's headline claim ("an idle
//! renderer's memory is reclaimed"). Five merged crates (`duet-core`,
//! `duet-runtime`, `duet-codec`, `duet-supervisor`, `duet-host`) implement
//! the state machines, policies and orchestration that claim it; the macOS
//! sibling of this example was the first thing that actually ran them
//! against a real engine and checked (`crates/duet-backend-macos`), the
//! Windows sibling re-measured it there, and this port runs the same
//! lifecycle against the real Linux engine (`libflutter_linux_gtk.so`, the
//! GTK embedder).
//!
//! An **example**, not a test: it needs a real `tao` event loop iterating
//! this thread's GTK main context for the engine's task runner, which
//! `cargo test`'s harness does not provide (Linux `tao` can build a loop off
//! the main thread — see `src/sink.rs`'s runnable closed-loop test — but a
//! booted engine still needs its creating thread iterating the main context
//! for the whole run, which a harness worker thread cannot promise).
//! `expect`/`unwrap` are used freely below for setup failures — a driver
//! that cannot boot its own event loop or find the Flutter fixture should
//! stop loudly, per the plan's rule for example code specifically (the
//! library itself has no such exemption).
//!
//! Run: `FLUTTER_LINUX_RENDERER=software GDK_BACKEND=x11 cargo run -p duet-backend-linux --example lifecycle`
//! (fixture: `cd spikes/spike_app && flutter build linux --debug`)
//!
//! `FLUTTER_LINUX_RENDERER=software` is required under WSLg, where no usable
//! GL reaches the embedder ("Could not determine GL version" — L-F4);
//! GL-capable desktops may omit it and let the embedder pick its own
//! renderer.
//!
//! # What this does and does not prove
//!
//! Verifiable here: the engine boots, a view attaches and renders (proven by
//! a pixel capture of the mapped window, not a human's eyes — see below),
//! and `ProxySink` marshals a real notification onto this thread (the spike
//! drove the same mechanism at 722/722 events over 180 s; macOS 709/709,
//! Windows 723/723). Under WSLg the window genuinely appears on the Windows
//! desktop through the RDP-backed compositor, but the run itself is
//! autonomous, with nobody at the keyboard, so the rendering proof still
//! comes from an artifact rather than a person looking. **Not** verifiable
//! here: real keyboard/mouse input — the spike's input criterion was
//! "cannot verify here" for exactly this reason (autonomous runs; the WSLg
//! windows are on a real desktop but nobody clicked them —
//! `spikes/spike-b-linux/FINDINGS.md`).
//!
//! # The macOS hang this example carries to Linux as an experiment
//!
//! On the macOS machine, running the sibling of this example as shipped
//! (`Policy::OnLastWindowClosed { grace_ms: 500 }`, against the `spike_app`
//! fixture) originally hung forever at [`Step::AwaitTeardown`] and never
//! reached [`Step::Report`] — `crates/duet-backend-macos/FINDINGS.md` (F1)
//! has the full writeup; the short version: `spike_app`'s `main.dart` runs a
//! `Ticker` that requests a new frame every vsync forever, regardless of
//! whether any view is attached. The moment detach removed the view
//! (`SurfaceAction::Suspend`, which every `OnLastWindowClosed` grace period
//! goes through before teardown), the engine's next frame attempt logged
//! `Could not create the embedder backing store` and retried — apparently
//! without ever yielding back to `tao`'s run loop. Sustained runs showed
//! 100 %+ CPU, RSS climbing rather than shrinking (222 MB -> 297 MB over
//! 90 s), and error lines arriving at up to ~13,000/s. The fix — real
//! `AppLifecycleState` sends on `flutter/lifecycle` around detach (macOS
//! FINDINGS F5) — is carried by this backend too: on Linux, detach sends
//! `inactive` then `hidden` and unmaps the window the view has lived in
//! since boot (hide, never unrealize — L-F3), and the view and engine stay
//! alive.
//!
//! Windows never showed the storm (W-F2), and neither did the Linux spike:
//! its W2 probe ran the full hide/show cycle *with* the lifecycle sends and
//! measured frames all but stopping while hidden (frameTicks advanced 16
//! over 4 s parked, then resumed on reattach — L-F3). Whether the storm
//! would reproduce without the sends was deliberately not measured on Linux
//! either: this run drives the exact configuration that hung macOS — the
//! same policy, the same `grace_ms: 500` Task 4 specifies, and the same
//! perpetually-ticking fixture, which is why this example alone defaults to
//! `spikes/spike_app` rather than `fixtures/duet_guest` — through the real
//! orchestration, and watches for the storm's signature. The backend sends
//! the lifecycle transitions either way, so a clean run is the expectation
//! here — and a hang would be a real finding, not a bug in this file.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant as StdInstant};

use duet_backend_linux::{DuetEvent, LinuxBackend, ProxySink};
use duet_core::{Instant as DuetInstant, Path, Policy, Value};
use duet_host::{BackendError, Host, Readiness, WindowBackend};
use duet_runtime::Runtime;
use duet_supervisor::{HostEvent, SurfaceId, WindowId};

use gtk::prelude::*;
use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::platform::unix::WindowExtUnix;

/// The surface's grace period, in milliseconds, between "last window closed"
/// and teardown. Short enough to keep the example fast, long enough to give
/// the "suspended" (view detached, engine alive) stage its own clearly
/// separated sample before teardown destroys the engine.
const GRACE_MS: u64 = 500;

/// Extra time to wait past `GRACE_MS` before ticking again, so the tick that
/// observes grace expiry is never racing the clock.
const GRACE_BUFFER_MS: u64 = 200;

/// Extra time given to the turn that rasterizes, before it runs: under
/// WSLg's software rasterizer the first frame can take noticeably longer
/// than one 50 ms turn to reach the mapped window. Warmup for a better
/// artifact, not a correctness requirement — the capture is non-fatal
/// either way.
const RASTER_WARMUP_MS: u64 = 1500;

/// The least of the engine's own cost that teardown must give back.
///
/// # Why a share, and not a number of kilobytes
///
/// The macOS sibling of this example used to assert an absolute floor of
/// 81,920 kB. That floor sat *inside* the range of values the example
/// legitimately produces, so whether it passed depended on which Flutter app
/// was booted — and this example's default fixture (`spikes/spike_app`) is
/// not the one every other example in this crate uses
/// (`fixtures/duet_guest`). Measured on the macOS machine, same binary, same
/// afternoon:
///
/// | Fixture | runs | reclaimed (kB) | 81,920 kB floor |
/// |---|---|---|---|
/// | `spikes/spike_app` (`runApp`, `MaterialApp`, a `Ticker`) | 3 | 122,560–124,112 | **passes** |
/// | `fixtures/duet_guest` (headless: no `runApp`, no widget tree) | 8 | 71,328–71,616 | **fails** |
///
/// Within each fixture the numbers are tight — 1,552 kB of spread across the
/// first, 288 kB across the second. So this was never a flaky test in the
/// usual sense of "same input, different answer". It was a test whose input
/// was ambiguous, asserting an absolute quantity that a *different guest app*
/// legitimately changes by 1.7×. Widening the number until both passed would
/// have set the floor below 71 MB, at which point it could no longer fail for
/// the reason it exists.
///
/// A share of the engine's own cost is the quantity that does not depend on
/// the app, because both halves of it scale together:
///
/// ```text
/// engine cost = (RSS while suspended) - (RSS before any engine existed)
/// reclaimed   = (RSS while suspended) - (RSS after teardown)
/// ```
///
/// Measured on macOS: 67.8–68.3 % for `spike_app`, 60.6 % for `duet_guest`.
/// On Linux the reclaim mechanism is `FlEngine`'s GObject *dispose*, which
/// `destroy_renderer` **forces** with `g_object_run_dispose` — waiting for a
/// last-reference drop does not get there, because one embedder-internal
/// reference outlives the view's destruction; the first run of this very
/// example is what measured that (5.4 % reclaimed, this assertion failing
/// exactly as designed) and drove the fix into the crate (see this crate's
/// engine docs and FINDINGS.md). The Linux spike watched frame cadence and
/// event delivery, not teardown memory, so no prior number existed on this
/// platform. A 50 % floor held on macOS with 10–18 points to spare, is
/// carried over unchanged, and still cannot pass if teardown reclaims
/// nothing — which is the only thing this assertion was ever for.
const MIN_RECLAIM_SHARE: f64 = 0.50;

/// The most of teardown's reclaim that detaching the view alone may account
/// for.
///
/// This is the *other* half of the claim: on Linux "detach" is
/// `gtk_widget_hide` of the window the view has lived in since boot (L-F3)
/// — an unmap frees no engine memory, and the engine object it would take
/// to free anything still holds every reference it had. What reclaims is
/// `FlEngine`'s dispose, forced inside `destroy_renderer` (see
/// [`MIN_RECLAIM_SHARE`]). The spike measured the suspended state's *behavior*
/// (frames all but stopped while hidden, resumed on show — L-F3's W2 cycle)
/// but took no RSS numbers around it, so like the reclaim floor above, the
/// detach share gets its first Linux measurement here. An assertion that
/// only checked the total drop would still pass if detach did all the work
/// and teardown did none — which would mean the whole suspend/teardown
/// distinction this framework is built on had quietly inverted.
///
/// Measured on the macOS machine: detach accounts for 1.8–4.0 % of what
/// teardown reclaims, across both fixtures. 20 % leaves five times that
/// margin, is carried over unchanged for this machine's first real run to
/// re-measure, and would still catch an inversion.
const MAX_DETACH_SHARE: f64 = 0.20;

/// Path to the Flutter `data` directory produced by
/// `flutter build linux --debug` (contains `flutter_assets/` and
/// `icudtl.dat`). Overridable via env var for other machines, matching the
/// Linux spike's fixture convention.
///
/// Canonicalized before use, keeping the three siblings' shape: on Linux
/// `FlutterEngine::boot` canonicalizes again on its own, so this mostly
/// makes the printed path absolute and unambiguous rather than rescuing the
/// engine from a cwd-relative one (which is what the Windows sibling needs
/// it for).
fn flutter_data_dir() -> String {
    let p = std::env::var("DUET_FLUTTER_BUNDLE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("spikes/spike_app/build/linux/x64/debug/bundle/data"));
    let p = p.canonicalize().unwrap_or(p);
    p.to_string_lossy().into_owned()
}

/// Reads this process's resident set size from the `VmRSS` line of
/// `/proc/self/status`, in kilobytes — the kernel's own accounting, plain
/// file I/O, no FFI. Returns -1 if the file or the line cannot be read, the
/// same "measurement failed" convention the other two siblings use (see
/// [`ratio`]).
///
/// Unlike the macOS and Windows siblings, this re-verifies no spike number:
/// the Linux spike watched frame cadence and event delivery, not teardown
/// memory (its only RSS note is "stable around 365 MB" across the 180 s
/// coexistence run), so the figures this function feeds the report are the
/// first RSS measurements of Duet's lifecycle claim on Linux.
fn rss_kb() -> i64 {
    let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
        return -1;
    };
    status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .and_then(|rest| rest.trim().strip_suffix("kB"))
        .and_then(|kb| kb.trim().parse().ok())
        .unwrap_or(-1)
}

/// One row of the RSS table this example prints at the end.
struct Sample {
    /// What stage of the lifecycle this was taken at.
    label: &'static str,
    /// RSS in kilobytes at that stage, or -1 if `/proc/self/status` could
    /// not be read.
    rss_kb: i64,
}

/// Delegates [`WindowBackend`] to a [`LinuxBackend`] shared (via
/// `Rc<RefCell<_>>`) with the driver loop below.
///
/// `duet_host::Host` takes ownership of its backend outright — by design, so
/// that a real backend's `&mut self` methods never need interior mutability
/// for `Host`'s own sake (see [`WindowBackend`]'s docs). But
/// [`LinuxBackend::open_window`] and [`LinuxBackend::close_window`] are
/// inherent methods a *driver* running inside the `tao` event loop must call
/// directly (see [`LinuxBackend`]'s docs on why window creation cannot go
/// through the trait), and that driver is this file, not `Host`. Sharing one
/// `LinuxBackend` between `Host` (through this wrapper) and the driver
/// (through the other `Rc` clone) resolves that without adding an accessor
/// to `duet-host`, which this phase does not touch.
///
/// Single-threaded only: every `FlutterEngine`/`tao` call must run on the
/// thread that owns the GTK main context — the thread running the `tao`
/// event loop here (see `duet_backend_linux`'s engine module docs) — so
/// `RefCell` costs nothing here that a real multi-threaded backend would not
/// already have paid for differently.
struct SharedBackend(Rc<RefCell<LinuxBackend>>);

impl WindowBackend for SharedBackend {
    fn start_renderer(&mut self, surface: SurfaceId) -> Result<Readiness, BackendError> {
        self.0.borrow_mut().start_renderer(surface)
    }

    fn attach_view(&mut self, surface: SurfaceId) -> Result<(), BackendError> {
        self.0.borrow_mut().attach_view(surface)
    }

    fn detach_view(&mut self, surface: SurfaceId) -> Result<(), BackendError> {
        self.0.borrow_mut().detach_view(surface)
    }

    fn destroy_renderer(&mut self, surface: SurfaceId) -> Result<(), BackendError> {
        self.0.borrow_mut().destroy_renderer(surface)
    }
}

/// Captures the Flutter view attached to `window` to a PNG at `path`, by
/// reading the mapped window's composited pixels back through
/// `gdk_pixbuf_get_from_window` — under WSLg, the composited X11 content of
/// the window, which is the software-rendered Flutter frame itself. This
/// machine's run is autonomous, with nobody at the keyboard, so the capture
/// is what turns "the view actually rendered" into an inspectable artifact
/// rather than a claim. Zero `unsafe`, unlike the sibling examples'
/// `PrintWindow` and `cacheDisplayInRect:` captures: gtk-rs owns every
/// handle.
///
/// Captures the whole surface window rather than hunting for a child
/// handle: there is no child HWND/NSView analog to find on Linux — the
/// engine's `FlView` fills the tao window's default vbox and never leaves
/// it (L-F3) — and this example only sees the crate's public API, same as
/// any other caller.
///
/// Returns the number of PNG bytes written, or a human-readable error.
fn rasterize_flutter_view(window: &tao::window::Window, path: &str) -> Result<usize, String> {
    let gtk_window = window.gtk_window();
    let gdk_window = gtk_window
        .window()
        .ok_or_else(|| "the GTK window has no GdkWindow - is it realized?".to_string())?;
    let (w, h) = (gdk_window.width(), gdk_window.height());
    if w <= 0 || h <= 0 {
        return Err("the window's GDK area is empty".to_string());
    }
    let pixbuf = gdk_window
        .pixbuf(0, 0, w, h)
        .ok_or_else(|| "gdk_pixbuf_get_from_window returned no pixels".to_string())?;
    pixbuf
        .savev(path, "png", &[])
        .map_err(|e| format!("writing {path} failed: {e}"))?;
    std::fs::metadata(path)
        .map(|meta| meta.len() as usize)
        .map_err(|e| format!("stat after writing {path} failed: {e}"))
}

/// The stages this example drives the surface through, one per run-loop
/// turn (at most one `Host::tick()` per turn keeps [`LinuxBackend`]'s detach
/// and re-attach in separate turns — the caller property the macOS backend
/// requires for the Flutter engine's one-view-per-engine-at-a-time rule.
/// Linux's attach/detach is `gtk_widget_show`/`hide` and the spike's W2
/// cycle gave no sign of needing turns between them, but the property holds
/// by construction and [`LinuxBackend`]'s docs ask drivers to keep honoring
/// it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    /// Sample RSS with no surface registered yet, then proceed at once.
    Init,
    /// Open the window, report it to the host, and tick — the host starts
    /// the renderer and attaches the view synchronously (`LinuxBackend`
    /// always reports [`Readiness::Ready`] for Flutter: Dart is serving by
    /// the time the boot realize returns — L-F2).
    OpenWindow,
    /// Capture the now-attached Flutter view to a PNG (this turn is given
    /// [`RASTER_WARMUP_MS`] of software-renderer warmup first).
    Rasterize,
    /// Report the window closed and tick — the host detaches the view but
    /// leaves the engine running, starting the grace period.
    CloseWindow,
    /// Wait out the grace period, then tick again — the host destroys the
    /// renderer, which is what should actually reclaim memory.
    AwaitTeardown,
    /// Print the table, assert the drop, and shut everything down.
    Report,
    /// Nothing left to do; the next turn exits the process.
    Done,
}

/// Everything the driver closure needs across turns of the event loop.
struct AppState {
    wall_start: StdInstant,
    step: Step,
    surface: SurfaceId,
    window_id: WindowId,
    /// The driver's own handle to the backend, for `open_window`/
    /// `close_window`/`window` — see [`SharedBackend`]'s docs for why this
    /// is a second handle rather than reaching through `host`.
    backend: Rc<RefCell<LinuxBackend>>,
    host: Host<SharedBackend>,
    /// `Some` until [`Step::Report`] shuts it down explicitly. `tao::EventLoop::run`
    /// never returns (`-> !`) and drops nothing on the way to `process::exit`
    /// (its own docs say so), so anything that needs a clean shutdown — the
    /// core thread, here — must be torn down before `ControlFlow::Exit` is
    /// set, not left to `Drop`.
    rt: Option<Runtime>,
    samples: Vec<Sample>,
    /// Set once an `Event::UserEvent(DuetEvent::Notifications(_))` carrying
    /// the probe write below is observed — proof `ProxySink` delivered.
    notifications_delivered: bool,
    zoom_path: Path,
}

impl AppState {
    fn now(&self) -> DuetInstant {
        DuetInstant(u64::try_from(self.wall_start.elapsed().as_millis()).unwrap_or(u64::MAX))
    }

    fn sample(&mut self, label: &'static str) {
        let kb = rss_kb();
        println!(
            "[t+{:>6.2}s rss={:>8}kB] {}",
            self.wall_start.elapsed().as_secs_f64(),
            kb,
            label
        );
        self.samples.push(Sample { label, rss_kb: kb });
    }
}

fn main() {
    let event_loop = EventLoopBuilder::<DuetEvent>::with_user_event().build();
    // The metronome. tao's GTK loop parks a pending `WaitUntil` inside a
    // fully blocking `gtk_main_iteration` with no timer source armed for the
    // deadline, so on a quiet session — no mapped window rendering frames —
    // a 50 ms turn stalls until some unrelated X11 event happens to wake the
    // context: measured at 11–16 s per turn under WSLg before this thread
    // existed. A proxy send is a glib channel source and *does* wake the
    // context, so a thread posting `DuetEvent::Tick` at turn cadence keeps
    // the deadline checks honest; the Tick events themselves are no-ops.
    let metronome = event_loop.create_proxy();
    std::thread::spawn(move || {
        while metronome.send_event(DuetEvent::Tick).is_ok() {
            std::thread::sleep(Duration::from_millis(50));
        }
    });
    let proxy = event_loop.create_proxy();
    let sink = ProxySink::new(proxy);

    let root = Value::map([("zoom", Value::Int(0))]);
    let rt = Runtime::spawn(root, sink);

    let data_dir = flutter_data_dir();
    println!("[lifecycle] Flutter data dir: {data_dir}");
    let backend = Rc::new(RefCell::new(LinuxBackend::new(data_dir)));

    let mut host = Host::new(rt.handle(), SharedBackend(backend.clone()));
    let surface = host.register(Policy::OnLastWindowClosed { grace_ms: GRACE_MS });
    let subscriber = host
        .subscriber_for(surface)
        .expect("a just-registered surface must have a subscriber");
    let zoom_path = Path::parse("zoom").expect("\"zoom\" is a valid path");
    rt.handle()
        .subscribe(subscriber, zoom_path.clone())
        .expect("subscribing on a live runtime should succeed");

    let mut state = AppState {
        wall_start: StdInstant::now(),
        step: Step::Init,
        surface,
        window_id: WindowId::new(1),
        backend,
        host,
        rt: Some(rt),
        samples: Vec::new(),
        notifications_delivered: false,
        zoom_path,
    };

    event_loop.run(move |event, target, control_flow| match event {
        Event::NewEvents(StartCause::Init) => {
            *control_flow = ControlFlow::WaitUntil(std::time::Instant::now());
        }
        Event::NewEvents(StartCause::ResumeTimeReached { .. }) => {
            advance(&mut state, target);
            *control_flow = match state.step {
                Step::Done => ControlFlow::Exit,
                // AwaitTeardown needs real time to pass, and Rasterize gets
                // extra warmup so the first software-rendered frame under
                // WSLg has actually reached the window before the capture
                // (see RASTER_WARMUP_MS); everything else is just waiting
                // for the GTK main context to process the previous step,
                // one run-loop turn at a time.
                Step::AwaitTeardown => ControlFlow::WaitUntil(
                    std::time::Instant::now() + Duration::from_millis(GRACE_MS + GRACE_BUFFER_MS),
                ),
                Step::Rasterize => ControlFlow::WaitUntil(
                    std::time::Instant::now() + Duration::from_millis(RASTER_WARMUP_MS),
                ),
                _ => ControlFlow::WaitUntil(std::time::Instant::now() + Duration::from_millis(50)),
            };
        }
        Event::UserEvent(DuetEvent::Notifications(batch)) => {
            println!(
                "[lifecycle] ProxySink delivered {} notification(s) onto the UI thread",
                batch.len()
            );
            if batch.iter().any(|n| n.patch.path == state.zoom_path) {
                state.notifications_delivered = true;
            }
        }
        Event::UserEvent(DuetEvent::Tick) => {}
        Event::LoopDestroyed => {
            println!("[lifecycle] LoopDestroyed");
        }
        _ => {}
    });
}

/// Advances the lifecycle by exactly one stage, called once per run-loop
/// turn. See [`Step`]'s docs for why "one tick per turn" matters here.
fn advance(state: &mut AppState, target: &tao::event_loop::EventLoopWindowTarget<DuetEvent>) {
    match state.step {
        Step::Init => {
            state.sample("process start, no surface registered");
            state.step = Step::OpenWindow;
        }

        Step::OpenWindow => {
            // Load-bearing order: open the window before the tick below. On
            // Linux `start_renderer` realizes the Flutter view INTO the
            // surface's window (created invisible; attach is what shows it)
            // — a renderer started with no window open boots headless into
            // a private holding window and can never be attached afterwards
            // (L-F1/L-F3).
            state
                .backend
                .borrow_mut()
                .open_window(state.surface, target, "Duet Lifecycle Example")
                .expect("opening the tao window should succeed");

            let now = state.now();
            state.host.handle_at(
                now,
                HostEvent::WindowOpened {
                    surface: state.surface,
                    window: state.window_id,
                },
            );
            let actions = state.host.tick(now);
            println!("[lifecycle] tick at open produced {actions:?}");
            state.sample("renderer started, view attached (Readiness::Ready)");

            // Prove ProxySink actually carries a real write across: this
            // triggers duet-runtime's core thread to call
            // ProxySink::deliver, which posts DuetEvent::Notifications onto
            // this event loop via the proxy captured at startup.
            state
                .rt
                .as_ref()
                .expect("runtime is present until Report")
                .handle()
                .set(&state.zoom_path, Value::Int(1))
                .expect("writing to a live runtime should succeed");

            state.step = Step::Rasterize;
        }

        Step::Rasterize => {
            let backend = state.backend.borrow();
            let window = backend
                .window(state.surface)
                .expect("the window opened in OpenWindow should still be open");
            let path = std::env::temp_dir()
                .join("duet_backend_linux_lifecycle.png")
                .to_string_lossy()
                .into_owned();
            match rasterize_flutter_view(window, &path) {
                Ok(bytes) => println!("[lifecycle] wrote {path} ({bytes} bytes)"),
                Err(e) => println!("[lifecycle] rasterization failed: {e}"),
            }
            drop(backend);
            state.sample("after rasterizing the attached view");
            state.step = Step::CloseWindow;
        }

        Step::CloseWindow => {
            let now = state.now();
            state.host.handle_at(
                now,
                HostEvent::WindowClosed {
                    surface: state.surface,
                    window: state.window_id,
                },
            );
            // The Suspend this tick produces hides the surface's window: on
            // Linux the view cannot leave the window it booted in (L-F3),
            // so detach unmaps the window itself. The tao window stays in
            // the backend's bookkeeping until Report calls close_window.
            let actions = state.host.tick(now);
            println!("[lifecycle] tick at close produced {actions:?}");
            state.sample("view detached, suspending (engine still alive)");
            state.step = Step::AwaitTeardown;
        }

        Step::AwaitTeardown => {
            let now = state.now();
            let actions = state.host.tick(now);
            println!("[lifecycle] tick after grace period produced {actions:?}");
            state.sample("torn down (engine shut down, if the grace period truly elapsed)");
            state.step = Step::Report;
        }

        Step::Report => {
            // The renderer is already destroyed (AwaitTeardown's tick), so
            // this close is physical on the spot — the deferred-close path
            // (closing under a live renderer) is not exercised here.
            let closed = state.backend.borrow_mut().close_window(state.surface);
            println!("[lifecycle] close_window logically closed an open window: {closed}");
            state.host.unregister(state.surface);

            print_report(state);

            state
                .rt
                .take()
                .expect("runtime is present until this step")
                .shutdown()
                .expect("the core thread should shut down cleanly");

            state.step = Step::Done;
        }

        Step::Done => {
            // Unreachable: the closure sets ControlFlow::Exit as soon as
            // this step is observed, so `advance` is never called again.
        }
    }
}

/// Prints the RSS table, the measured shares, and PASS/FAIL, then asserts
/// they clear [`MIN_RECLAIM_SHARE`] and [`MAX_DETACH_SHARE`]. A failed
/// assertion here is a finding about the framework, not a bug in this
/// example — worth a FINDINGS.md entry, as the macOS hang got.
fn print_report(state: &AppState) {
    println!();
    println!("=== Duet backend-linux lifecycle RSS report ===");
    println!(
        "ProxySink delivered a real notification onto the UI thread: {}",
        state.notifications_delivered
    );
    println!();
    let width = state
        .samples
        .iter()
        .map(|s| s.label.len())
        .max()
        .unwrap_or(0);
    for s in &state.samples {
        println!("{:width$}  {:>8} kB", s.label, s.rss_kb, width = width);
    }
    println!();

    // Measure from the last sample before teardown, not from the moment the
    // view was first attached.
    //
    // The attach sample lands well before the raster backend and the Dart
    // heap have warmed up — measured on macOS at ~166 MB against a settled
    // ~226 MB. Using it understates the reclaim by roughly 60 MB and
    // answers the wrong question: what matters is how much a *live,
    // rendering* surface costs versus one that has been torn down. The
    // macOS and Windows measurements both used a settled pre-teardown
    // baseline; keeping it here keeps the three reports comparable.
    let start = find_sample(state, "process start, no surface registered");
    let peak = find_sample(state, "after rasterizing the attached view");
    let suspended = find_sample(state, "view detached, suspending (engine still alive)");
    let torn_down = find_sample(
        state,
        "torn down (engine shut down, if the grace period truly elapsed)",
    );

    // What the engine cost above an engine-less process, and what came back.
    let engine_cost = suspended - start;
    let reclaimed = suspended - torn_down;
    let by_detach = peak - suspended;

    // Both shares are computed against `engine_cost` and `reclaimed`, which
    // are measured in this same run against this same app — which is exactly
    // why they do not move when the app does. Guarded against a zero
    // denominator rather than trusting the sample: a failed /proc read
    // returning -1 would otherwise produce a NaN that compares false and
    // reports as a failure with no explanation.
    let reclaim_share = ratio(reclaimed, engine_cost);
    let detach_share = ratio(by_detach, reclaimed);

    println!("absolute, for the record:");
    println!("  the engine cost      {engine_cost:>8} kB above an engine-less process");
    println!("  detaching the view   {by_detach:>8} kB back");
    println!("  tearing down         {reclaimed:>8} kB back");
    println!();
    println!(
        "teardown reclaimed {:.1}% of what the engine cost (floor {:.0}%)",
        reclaim_share * 100.0,
        MIN_RECLAIM_SHARE * 100.0
    );
    println!(
        "detaching accounted for {:.1}% of that (ceiling {:.0}%)",
        detach_share * 100.0,
        MAX_DETACH_SHARE * 100.0
    );
    println!();

    let enough = reclaim_share >= MIN_RECLAIM_SHARE;
    let shutdown_did_it = detach_share <= MAX_DETACH_SHARE;
    for (ok, label) in [
        (enough, "teardown gives back most of what the engine cost"),
        (
            shutdown_did_it,
            "FlEngine's dispose is what reclaims it, not detaching the view",
        ),
    ] {
        println!("{}: {label}", if ok { "PASS" } else { "FAIL" });
    }
    println!();

    assert!(
        enough,
        "teardown must reclaim at least {:.0}% of the engine's own cost; measured {:.1}% \
         ({reclaimed} kB of {engine_cost} kB, suspended={suspended} kB, \
         torn_down={torn_down} kB, start={start} kB) — a finding to write down, not a bug here",
        MIN_RECLAIM_SHARE * 100.0,
        reclaim_share * 100.0
    );
    assert!(
        shutdown_did_it,
        "detaching the view must not be what reclaims memory — on Linux detach is \
         gtk_widget_hide, and the embedder's shutdown runs in FlEngine's dispose, reached in \
         destroy_renderer; detach accounted for {:.1}% of the reclaim ({by_detach} kB of \
         {reclaimed} kB), over the {:.0}% ceiling",
        detach_share * 100.0,
        MAX_DETACH_SHARE * 100.0
    );
}

/// `part / whole` as a share, or 0.0 if `whole` is not positive.
///
/// `rss_kb` returns -1 when `/proc/self/status` could not be read, and a
/// negative or zero denominator would produce an infinity or a NaN. A NaN
/// compares false against every threshold, so it would report as a failure —
/// correct by accident, but with a message naming a nonsense percentage.
/// Zero fails the floor and passes the ceiling, both of which are the honest
/// answers to "the measurement did not work".
fn ratio(part: i64, whole: i64) -> f64 {
    if whole <= 0 {
        return 0.0;
    }
    part as f64 / whole as f64
}

/// Looks up a sample by its exact label, for the two the report needs to
/// diff. Panics if missing — every label here is one this file itself wrote
/// two steps earlier in the same run, so its absence would mean the
/// lifecycle never reached that stage, which is itself worth stopping loudly
/// for rather than reporting a silently wrong delta.
fn find_sample(state: &AppState, label: &str) -> i64 {
    state
        .samples
        .iter()
        .find(|s| s.label == label)
        .unwrap_or_else(|| panic!("expected a sample labelled {label:?}"))
        .rss_kb
}
