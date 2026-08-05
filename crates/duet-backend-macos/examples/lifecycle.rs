//! The RSS proof: drives one real Flutter surface through a full lifecycle —
//! window open, engine boot, view attach, suspend, teardown — over the real
//! `duet-host` orchestration, and measures this process's resident set size
//! at every stage.
//!
//! This is the first time Duet's headline claim ("an idle renderer's memory
//! is reclaimed") can be *measured* rather than asserted structurally. Five
//! merged crates (`duet-core`, `duet-runtime`, `duet-codec`,
//! `duet-supervisor`, `duet-host`) implement the state machines, policies and
//! orchestration that claim it; this example is the first thing that
//! actually runs them against a real engine and checks.
//!
//! An **example**, not a test: it needs the main thread and a real `tao`
//! event loop, neither of which `cargo test`'s harness provides (see
//! `src/sink.rs`'s `#[ignore]`d test for the same constraint in miniature).
//! `expect`/`unwrap` are used freely below for setup failures — a driver
//! that cannot boot its own event loop or find the Flutter fixture should
//! stop loudly, per the plan's rule for example code specifically (the
//! library itself has no such exemption).
//!
//! Run: `cargo run -p duet-backend-macos --example lifecycle`
//!
//! # What this does and does not prove
//!
//! Verifiable here, all without a reachable on-screen WindowServer (see the
//! crate root docs): the engine boots, a view attaches and renders (proven
//! by in-process rasterization, not a screenshot), and `ProxySink` marshals
//! a real notification onto this thread. **Not** verifiable here: anything a
//! human would judge by looking at a screen, or real keyboard/mouse input —
//! see `FINDINGS.md`.
//!
//! # A real hang this example reproduces, not a bug in this file
//!
//! On this machine, running this example as shipped (`Policy::OnLastWindowClosed
//! { grace_ms: 500 }`, against the `spike_app` fixture) reliably hangs at
//! [`Step::AwaitTeardown`] and never reaches [`Step::Report`] — confirmed
//! across four independent runs, one held open for nearly five minutes.
//! `FINDINGS.md` has the full writeup; the short version: `spike_app`'s
//! `main.dart` runs a `Ticker` that requests a new frame every vsync forever,
//! regardless of whether any view is attached. The moment
//! [`duet_backend_macos::MacBackend::detach_view`] removes the view
//! (`SurfaceAction::Suspend`, which every `OnLastWindowClosed` grace period
//! goes through before teardown), the engine's next frame attempt logs
//! `Could not create the embedder backing store` and retries — apparently
//! without ever yielding back to `tao`'s run loop, since this process's own
//! `[lifecycle]` progress lines and `Host::tick` calls stop dead at that
//! point. Sustained runs showed 100 %+ CPU, RSS climbing rather than
//! shrinking (222 MB -> 297 MB over 90 s), and error lines arriving at up to
//! ~13,000/s. Reducing the real wall-clock time spent in that state (by
//! ticking the very next run-loop turn with a `duet_core::Instant` already
//! past the grace deadline, rather than actually waiting `grace_ms`) did
//! **not** avoid it — the storm was already established by the time that
//! turn ran. This is left as the plan's own required, honest hang rather
//! than papered over: `grace_ms: 500` here is the exact value Task 4
//! specifies, and it is not this example's place to dodge a real finding by
//! quietly changing it.

use std::cell::RefCell;
use std::process::Command;
use std::rc::Rc;
use std::time::{Duration, Instant as StdInstant};

use duet_backend_macos::{DuetEvent, MacBackend, ProxySink};
use duet_core::{Instant as DuetInstant, Path, Policy, Value};
use duet_host::{BackendError, Host, Readiness, WindowBackend};
use duet_runtime::Runtime;
use duet_supervisor::{HostEvent, SurfaceId, WindowId};

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{NSBitmapImageFileType, NSView, NSWindow};
use objc2_foundation::{NSDictionary, NSString};
use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::platform::macos::WindowExtMacOS;

/// The surface's grace period, in milliseconds, between "last window closed"
/// and teardown. Short enough to keep the example fast, long enough to give
/// the "suspended" (view detached, engine alive) stage its own clearly
/// separated sample before teardown destroys the engine.
const GRACE_MS: u64 = 500;

/// Extra time to wait past `GRACE_MS` before ticking again, so the tick that
/// observes grace expiry is never racing the clock.
const GRACE_BUFFER_MS: u64 = 200;

/// The minimum acceptable drop between "view attached" and "after teardown",
/// in kilobytes as reported by `ps -o rss=`. Spike A measured a 119,296 kB
/// drop (223 MB attached -> 104 MB after `shutDownEngine`); 80 MB is a
/// generous floor that still cannot pass if teardown reclaims nothing.
const MIN_DROP_KB: i64 = 80 * 1024;

/// Path to the Flutter `App.framework` produced by `flutter build macos`.
/// Overridable via env var for other machines, matching Spike A's fixture
/// convention.
fn app_framework_path() -> String {
    std::env::var("DUET_APP_FRAMEWORK_PATH").unwrap_or_else(|_| {
        "spikes/spike_app/build/macos/Build/Products/Debug/App.framework".to_string()
    })
}

/// Reads this process's resident set size via `ps`, in kilobytes. Ported
/// verbatim in spirit from Spike A's `rss_kb()`
/// (`spikes/spike-a-macos/src/main.rs`), which used the same call to measure
/// the 223 MB -> 104 MB drop this example re-verifies through the real
/// `duet-host` orchestration instead of hand-rolled ObjC calls.
fn rss_kb() -> i64 {
    let pid = std::process::id().to_string();
    Command::new("ps")
        .args(["-o", "rss=", "-p", &pid])
        .output()
        .ok()
        .and_then(|out| String::from_utf8_lossy(&out.stdout).trim().parse().ok())
        .unwrap_or(-1)
}

/// One row of the RSS table this example prints at the end.
struct Sample {
    /// What stage of the lifecycle this was taken at.
    label: &'static str,
    /// RSS in kilobytes at that stage, or -1 if `ps` could not be read.
    rss_kb: i64,
}

/// Delegates [`WindowBackend`] to a [`MacBackend`] shared (via `Rc<RefCell<_>>`)
/// with the driver loop below.
///
/// `duet_host::Host` takes ownership of its backend outright — by design, so
/// that a real backend's `&mut self` methods never need interior mutability
/// for `Host`'s own sake (see [`WindowBackend`]'s docs). But
/// [`MacBackend::open_window`] and [`MacBackend::close_window`] are inherent
/// methods a *driver* running inside the `tao` event loop must call directly
/// (see [`MacBackend`]'s docs on why window creation cannot go through the
/// trait), and that driver is this file, not `Host`. Sharing one `MacBackend`
/// between `Host` (through this wrapper) and the driver (through the other
/// `Rc` clone) resolves that without adding an accessor to `duet-host`,
/// which this phase does not touch.
///
/// Single-threaded only: every `FlutterEngine`/`tao` call must run on the
/// main thread regardless (see `duet_backend_macos::engine`'s module docs),
/// so `RefCell` costs nothing here that a real multi-threaded backend would
/// not already have paid for differently.
struct SharedBackend(Rc<RefCell<MacBackend>>);

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

/// Rasterizes the Flutter view attached to `window` to a PNG at `path`, using
/// AppKit's own in-process rendering rather than a screen capture. This
/// machine has no reachable on-screen WindowServer for spawned processes
/// (see the crate root docs), so `cacheDisplayInRect:toBitmapImageRep:` is
/// the only way to prove the view actually rendered here. Ported from Spike
/// A's `snapshot_view_to_png` (`spikes/spike-a-macos/src/main.rs`), which
/// used the same call to capture real PNGs of the Flutter counter app.
///
/// Finds the Flutter view as the content view's first subview rather than
/// reaching into `duet-backend-macos`'s private engine handle: this example
/// only sees the crate's public API, same as any other caller, and
/// [`MacBackend::attach_view`] (via `duet_backend_macos::engine`) parents
/// exactly one subview into the window's content view.
///
/// Returns the number of PNG bytes written, or a human-readable error.
fn rasterize_flutter_view(window: &tao::window::Window, path: &str) -> Result<usize, String> {
    let ns_window_ptr = window.ns_window() as *mut NSWindow;
    if ns_window_ptr.is_null() {
        return Err("tao window has no backing NSWindow".to_string());
    }
    // SAFETY: `ns_window_ptr` is non-null (checked above) and, per
    // `tao::platform::macos::WindowExtMacOS`'s contract, points at the live
    // `NSWindow*` backing `window` for as long as `window` itself is alive,
    // which this function's borrow of `window` guarantees for this call.
    let ns_window: &NSWindow = unsafe { &*ns_window_ptr };
    let content_view = ns_window
        .contentView()
        .ok_or_else(|| "NSWindow has no contentView".to_string())?;

    let subviews = content_view.subviews();
    let flutter_view: Retained<NSView> = subviews
        .into_iter()
        .next()
        .ok_or_else(|| "content view has no subviews - was attach_view called?".to_string())?;

    let bounds = flutter_view.bounds();
    flutter_view.setNeedsDisplay(true);
    flutter_view.displayIfNeeded();

    let bitmap = flutter_view
        .bitmapImageRepForCachingDisplayInRect(bounds)
        .ok_or_else(|| "bitmapImageRepForCachingDisplayInRect returned nil".to_string())?;
    flutter_view.cacheDisplayInRect_toBitmapImageRep(bounds, &bitmap);

    let props: Retained<NSDictionary<objc2_app_kit::NSBitmapImageRepPropertyKey, AnyObject>> =
        NSDictionary::new();
    // SAFETY: `bitmap` is a live `NSBitmapImageRep` that
    // `cacheDisplayInRect_toBitmapImageRep` just populated above;
    // `representationUsingType:properties:` is safe to send to any live,
    // populated bitmap rep with a valid (possibly empty) properties
    // dictionary.
    let png_data =
        unsafe { bitmap.representationUsingType_properties(NSBitmapImageFileType::PNG, &props) }
            .ok_or_else(|| "representationUsingType:properties: returned nil".to_string())?;

    let bytes = png_data.len();
    let path_ns = NSString::from_str(path);
    if !png_data.writeToFile_atomically(&path_ns, true) {
        return Err(format!("writeToFile:atomically: failed for {path}"));
    }
    Ok(bytes)
}

/// The stages this example drives the surface through, one per run-loop
/// turn (see the module docs on why: at most one `Host::tick()` per turn is
/// what keeps `MacBackend`'s detach and re-attach in separate turns, which
/// the Flutter engine's one-view-per-engine-at-a-time rule requires).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    /// Sample RSS with no surface registered yet, then proceed at once.
    Init,
    /// Open the window, report it to the host, and tick — the host starts
    /// the renderer and attaches the view synchronously (`MacBackend`
    /// always reports [`Readiness::Ready`] for Flutter).
    OpenWindow,
    /// Rasterize the now-attached Flutter view to a PNG.
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
    backend: Rc<RefCell<MacBackend>>,
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
    let proxy = event_loop.create_proxy();
    let sink = ProxySink::new(proxy);

    let root = Value::map([("zoom", Value::Int(0))]);
    let rt = Runtime::spawn(root, sink);

    let framework = app_framework_path();
    println!("[lifecycle] App.framework path: {framework}");
    let backend = Rc::new(RefCell::new(MacBackend::new(framework)));

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
                // AwaitTeardown needs real time to pass; everything else is
                // just waiting for Cocoa to process the previous step, one
                // run-loop turn at a time.
                Step::AwaitTeardown => ControlFlow::WaitUntil(
                    std::time::Instant::now() + Duration::from_millis(GRACE_MS + GRACE_BUFFER_MS),
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
            match rasterize_flutter_view(window, "/tmp/duet_backend_macos_lifecycle.png") {
                Ok(bytes) => println!(
                    "[lifecycle] wrote /tmp/duet_backend_macos_lifecycle.png ({bytes} bytes)"
                ),
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
            let closed = state.backend.borrow_mut().close_window(state.surface);
            println!("[lifecycle] close_window removed a tao window: {closed}");
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

/// Prints the RSS table, the measured delta, and PASS/FAIL, then asserts the
/// delta clears [`MIN_DROP_KB`]. A failed assertion here is a finding about
/// the framework, not a bug in this example — see the crate's `FINDINGS.md`.
fn print_report(state: &AppState) {
    println!();
    println!("=== Duet backend-macos lifecycle RSS report ===");
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
    // The attach sample lands ~0.3s in, before Skia and the Dart heap have
    // warmed up — measured at ~160 MB against a settled ~223 MB. Using it
    // understates the reclaim by roughly 60 MB and answers the wrong question:
    // what matters is how much a *live, rendering* surface costs versus one
    // that has been torn down. Spike A used the same settled baseline
    // (223 MB -> 104 MB).
    let settled = find_sample(state, "view detached, suspending (engine still alive)");
    let torn_down = find_sample(
        state,
        "torn down (engine shut down, if the grace period truly elapsed)",
    );
    let delta = settled - torn_down;
    println!(
        "delta (settled, pre-teardown -> after teardown): {delta} kB \
         (floor: {MIN_DROP_KB} kB / 80 MB)"
    );

    if delta >= MIN_DROP_KB {
        println!(
            "PASS: RSS dropped by {delta} kB after teardown, clearing the {MIN_DROP_KB} kB floor"
        );
    } else {
        println!(
            "FAIL: RSS dropped by only {delta} kB after teardown, below the {MIN_DROP_KB} kB floor"
        );
    }
    println!();

    assert!(
        delta >= MIN_DROP_KB,
        "teardown must reclaim at least {MIN_DROP_KB} kB (80 MB); measured {delta} kB \
         (settled={settled} kB, torn_down={torn_down} kB) — see FINDINGS.md"
    );
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
