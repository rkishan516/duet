//! The RSS proof: drives one real Flutter surface through a full lifecycle —
//! window open, engine boot, view attach, suspend, teardown — over the real
//! `duet-host` orchestration, and measures this process's resident set size
//! at every stage.
//!
//! This is the Windows re-measurement of Duet's headline claim ("an idle
//! renderer's memory is reclaimed"). Five merged crates (`duet-core`,
//! `duet-runtime`, `duet-codec`, `duet-supervisor`, `duet-host`) implement
//! the state machines, policies and orchestration that claim it; the macOS
//! sibling of this example was the first thing that actually ran them
//! against a real engine and checked (`crates/duet-backend-macos`), and this
//! port runs the same lifecycle against the real Windows engine
//! (`flutter_windows.dll`).
//!
//! An **example**, not a test: it needs a real `tao` event loop pumping this
//! thread's messages for the engine's task runner, which `cargo test`'s
//! harness does not provide (Windows `tao` can build a loop off the main
//! thread — see `src/sink.rs`'s runnable closed-loop test — but a booted
//! engine still needs its creating thread pumping messages for the whole
//! run, which a harness worker thread cannot promise). `expect`/`unwrap`
//! are used freely below for setup failures — a driver that cannot boot its
//! own event loop or find the Flutter fixture should stop loudly, per the
//! plan's rule for example code specifically (the library itself has no such
//! exemption).
//!
//! Run: `cargo run -p duet-backend-windows --example lifecycle`
//! (fixture: `cd spikes/spike_app && flutter build windows --debug`)
//!
//! # What this does and does not prove
//!
//! Verifiable here: the engine boots, a view attaches and renders (proven by
//! in-process rasterization, not a screenshot — see below), and `ProxySink`
//! marshals a real notification onto this thread. Unlike every macOS
//! measurement in this project (taken on a machine with no reachable
//! on-screen WindowServer), this machine has a real display session — the
//! window genuinely appears on a physical screen — but the run itself is
//! autonomous, with nobody at the keyboard, so the rendering proof still
//! comes from an artifact rather than a human's eyes. **Not** verifiable
//! here: real keyboard/mouse input — the spike found posted synthetic input
//! reaches Flutter's WndProc (but not WebView2), which proves delivery, not
//! full routing; real input needs a person at the machine (see
//! `spikes/spike-b-windows/FINDINGS.md` W-F7).
//!
//! # The macOS hang this example carries to Windows as an experiment
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
//! FINDINGS F5) — is carried by this backend too: on Windows, detach parks
//! the view HWND in a hidden window with the same lifecycle sends around it
//! (W-F2), and the controller and engine stay alive.
//!
//! On Windows the storm has never been observed: the spike's W2 probe ran a
//! full detach/park/reattach cycle *with* the lifecycle sends and saw no
//! backing-store storm (W-F2). Whether the storm would reproduce here
//! without them was deliberately left unmeasured by the spike and assigned
//! to this example (`spikes/spike-b-windows/FINDINGS.md`, "What could not be
//! verified here"): this run drives the exact configuration that hung macOS
//! — the same policy, the same `grace_ms: 500` Task 4 specifies, and the
//! same perpetually-ticking fixture, which is why this example alone
//! defaults to `spikes/spike_app` rather than `fixtures/duet_guest` —
//! through the real orchestration, and watches for the storm's signature
//! (and for the framework lifecycle assertions W-F10 warns the Windows
//! embedder's own activation-driven sends could provoke). The backend sends
//! the lifecycle transitions either way, so a clean run is the expectation
//! here — and a hang would be a real finding, not a bug in this file.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant as StdInstant};

use duet_backend_windows::{DuetEvent, ProxySink, WinBackend};
use duet_core::{Instant as DuetInstant, Path, Policy, Value};
use duet_host::{BackendError, Host, Readiness, WindowBackend};
use duet_runtime::Runtime;
use duet_supervisor::{HostEvent, SurfaceId, WindowId};

use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::platform::windows::WindowExtWindows;

use windows_sys::Win32::Foundation::{HWND, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleBitmap, CreateCompatibleDC,
    DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, GetDIBits, ReleaseDC, SelectObject,
};
use windows_sys::Win32::Storage::Xps::PrintWindow;
use windows_sys::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
use windows_sys::Win32::System::Threading::GetCurrentProcess;
use windows_sys::Win32::UI::WindowsAndMessaging::{GW_CHILD, GetClientRect, GetWindow};

/// The surface's grace period, in milliseconds, between "last window closed"
/// and teardown. Short enough to keep the example fast, long enough to give
/// the "suspended" (view detached, engine alive) stage its own clearly
/// separated sample before teardown destroys the engine.
const GRACE_MS: u64 = 500;

/// Extra time to wait past `GRACE_MS` before ticking again, so the tick that
/// observes grace expiry is never racing the clock.
const GRACE_BUFFER_MS: u64 = 200;

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
/// The Windows spike's one teardown datapoint agrees comfortably: destroying
/// the view controller — which owns the engine on Windows — dropped RSS
/// 258,080 kB -> 78,444 kB (W-F1). A 50 % floor held on macOS with 10–18
/// points to spare, is carried over unchanged for this machine's first real
/// run to re-measure, and still cannot pass if teardown reclaims nothing —
/// which is the only thing this assertion was ever for.
const MIN_RECLAIM_SHARE: f64 = 0.50;

/// The most of teardown's reclaim that detaching the view alone may account
/// for.
///
/// This is the *other* half of the claim, and the one the Windows spike
/// actually established: **`FlutterDesktopViewControllerDestroy` is what
/// frees memory, not parking the view.** The spike measured RSS flat around
/// 255 MB across a detach-as-park (W-F2) and 258 MB -> 78 MB at destroy
/// (W-F1) — the same shape Spike A measured on macOS, where 223 MB stood
/// before and after a detach. An assertion that only checked the total drop
/// would still pass if detach did all the work and teardown did none — which
/// would mean the whole suspend/teardown distinction this framework is built
/// on had quietly inverted.
///
/// Measured on the macOS machine: detach accounts for 1.8–4.0 % of what
/// teardown reclaims, across both fixtures. 20 % leaves five times that
/// margin, is carried over unchanged for this machine's first real run to
/// re-measure, and would still catch an inversion.
const MAX_DETACH_SHARE: f64 = 0.20;

/// Newer-SDK `PrintWindow` flag missing from win32metadata, declared by
/// hand: composes DirectComposition/DWM-rendered content (Flutter's ANGLE
/// swapchain included) into the capture instead of black (W-F9).
const PW_RENDERFULLCONTENT: u32 = 2;

/// Path to the Flutter `data` directory produced by
/// `flutter build windows --debug` (contains `flutter_assets/` and
/// `icudtl.dat`). Overridable via env var for other machines, matching the
/// Windows spike's fixture convention.
///
/// Canonicalized before use: the engine resolves relative paths against the
/// EXE, not the cwd (see the spike's `flutter_data_dir`,
/// `spikes/spike-b-windows/src/main.rs`), so a relative default would pass
/// this crate's own existence checks and then fail inside the engine.
fn flutter_data_dir() -> String {
    let p = std::env::var("DUET_FLUTTER_BUNDLE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("spikes/spike_app/build/windows/x64/runner/Debug/data"));
    let p = p.canonicalize().unwrap_or(p);
    p.to_string_lossy().into_owned()
}

/// Reads this process's resident set size via `K32GetProcessMemoryInfo`
/// (`WorkingSetSize`), in kilobytes. Ported verbatim in spirit from the
/// Windows spike's `rss_kb_num()` (`spikes/spike-b-windows/src/main.rs`),
/// which used the same call to measure the 258 MB -> 78 MB drop (W-F1) this
/// example re-verifies through the real `duet-host` orchestration instead of
/// hand-rolled FFI calls.
fn rss_kb() -> i64 {
    // SAFETY: a plain query of this process's own memory counters; `pmc` is
    // a zeroed POD with `cb` set to its true size, and `GetCurrentProcess`
    // returns a pseudo-handle that needs no closing.
    unsafe {
        let mut pmc: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
        pmc.cb = size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        if K32GetProcessMemoryInfo(GetCurrentProcess(), &mut pmc, pmc.cb) != 0 {
            (pmc.WorkingSetSize / 1024) as i64
        } else {
            -1
        }
    }
}

/// One row of the RSS table this example prints at the end.
struct Sample {
    /// What stage of the lifecycle this was taken at.
    label: &'static str,
    /// RSS in kilobytes at that stage, or -1 if the memory counters could
    /// not be read.
    rss_kb: i64,
}

/// Delegates [`WindowBackend`] to a [`WinBackend`] shared (via `Rc<RefCell<_>>`)
/// with the driver loop below.
///
/// `duet_host::Host` takes ownership of its backend outright — by design, so
/// that a real backend's `&mut self` methods never need interior mutability
/// for `Host`'s own sake (see [`WindowBackend`]'s docs). But
/// [`WinBackend::open_window`] and [`WinBackend::close_window`] are inherent
/// methods a *driver* running inside the `tao` event loop must call directly
/// (see [`WinBackend`]'s docs on why window creation cannot go through the
/// trait), and that driver is this file, not `Host`. Sharing one `WinBackend`
/// between `Host` (through this wrapper) and the driver (through the other
/// `Rc` clone) resolves that without adding an accessor to `duet-host`,
/// which this phase does not touch.
///
/// Single-threaded only: every `FlutterEngine`/`tao` call must run on the
/// thread that created the engine — the thread running the `tao` event loop
/// here (see `duet_backend_windows`'s engine module docs) — so `RefCell`
/// costs nothing here that a real multi-threaded backend would not already
/// have paid for differently.
struct SharedBackend(Rc<RefCell<WinBackend>>);

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

/// Rasterizes the Flutter view attached to `window` to a BMP at `path`,
/// using `PrintWindow` with [`PW_RENDERFULLCONTENT`] — in-process rendering,
/// not a screen capture. This machine does have a real display session
/// (unlike the macOS measurements), but the run is autonomous, so an
/// in-process rasterization is still what turns "the view actually rendered"
/// into an inspectable artifact rather than a claim; without the flag,
/// DirectComposition content captures as black (W-F9). Ported from the
/// Windows spike's `snapshot_hwnd_to_bmp`
/// (`spikes/spike-b-windows/src/main.rs`), which used the same call to
/// capture real pixels of both guests into `evidence/*.bmp`.
///
/// Finds the Flutter view as the tao window's first child HWND rather than
/// reaching into `duet-backend-windows`'s private engine handle: this
/// example only sees the crate's public API, same as any other caller, and
/// [`WinBackend`]'s `attach_view` (via its engine) parents exactly one child
/// HWND into the window.
///
/// Returns the number of BMP bytes written, or a human-readable error.
fn rasterize_flutter_view(window: &tao::window::Window, path: &str) -> Result<usize, String> {
    let parent = window.hwnd() as HWND;
    if parent.is_null() {
        return Err("tao window has no backing HWND".to_string());
    }
    // SAFETY: `parent` is non-null (checked above) and, per
    // `tao::platform::windows::WindowExtWindows`'s contract, is the live HWND
    // backing `window` for as long as `window` itself is alive, which this
    // function's borrow of `window` guarantees for this call. `GetWindow` is
    // a plain handle query on the calling thread.
    let flutter_view = unsafe { GetWindow(parent, GW_CHILD) };
    if flutter_view.is_null() {
        return Err("window has no child HWND - was attach_view called?".to_string());
    }

    // SAFETY: `flutter_view` is the live child window found above, owned by
    // this thread; every GDI call below runs against handles created in this
    // same block, and each is released before the block ends.
    unsafe {
        let mut rect = std::mem::zeroed::<RECT>();
        if GetClientRect(flutter_view, &mut rect) == 0 {
            return Err("GetClientRect failed for the Flutter view".to_string());
        }
        let (w, h) = (rect.right - rect.left, rect.bottom - rect.top);
        if w <= 0 || h <= 0 {
            return Err("the Flutter view's client rect is empty".to_string());
        }
        let hdc_window = GetDC(flutter_view);
        let hdc_mem = CreateCompatibleDC(hdc_window);
        let hbm = CreateCompatibleBitmap(hdc_window, w, h);
        let old = SelectObject(hdc_mem, hbm);
        let printed = PrintWindow(flutter_view, hdc_mem, PW_RENDERFULLCONTENT);

        let mut bmi: BITMAPINFO = std::mem::zeroed();
        bmi.bmiHeader = BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: h, // positive: bottom-up, matching BMP layout
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            biSizeImage: 0,
            biXPelsPerMeter: 0,
            biYPelsPerMeter: 0,
            biClrUsed: 0,
            biClrImportant: 0,
        };
        let mut pixels = vec![0u8; (w as usize) * (h as usize) * 4];
        let got = GetDIBits(
            hdc_mem,
            hbm,
            0,
            h as u32,
            pixels.as_mut_ptr().cast(),
            &mut bmi,
            DIB_RGB_COLORS,
        );

        SelectObject(hdc_mem, old);
        DeleteObject(hbm);
        DeleteDC(hdc_mem);
        ReleaseDC(flutter_view, hdc_window);

        if printed == 0 {
            return Err("PrintWindow(PW_RENDERFULLCONTENT) failed".to_string());
        }
        if got == 0 {
            return Err("GetDIBits returned no scan lines".to_string());
        }
        write_bmp(path, w, h, &pixels).map_err(|e| format!("writing {path} failed: {e}"))
    }
}

/// Writes 32-bit bottom-up BGRA pixels as a plain BMP file, returning the
/// number of bytes written. The exact writer the Windows spike validated
/// against real captured frames (`spikes/spike-b-windows/src/main.rs`).
fn write_bmp(path: &str, w: i32, h: i32, pixels: &[u8]) -> std::io::Result<usize> {
    use std::io::Write;
    let mut f = std::fs::File::create(path)?;
    let image_size = (w as u32) * (h as u32) * 4;
    let off: u32 = 14 + 40;
    f.write_all(b"BM")?;
    f.write_all(&(off + image_size).to_le_bytes())?;
    f.write_all(&0u32.to_le_bytes())?;
    f.write_all(&off.to_le_bytes())?;
    f.write_all(&40u32.to_le_bytes())?;
    f.write_all(&w.to_le_bytes())?;
    f.write_all(&h.to_le_bytes())?;
    f.write_all(&1u16.to_le_bytes())?;
    f.write_all(&32u16.to_le_bytes())?;
    f.write_all(&0u32.to_le_bytes())?; // BI_RGB
    f.write_all(&image_size.to_le_bytes())?;
    f.write_all(&2835i32.to_le_bytes())?;
    f.write_all(&2835i32.to_le_bytes())?;
    f.write_all(&0u32.to_le_bytes())?;
    f.write_all(&0u32.to_le_bytes())?;
    f.write_all(pixels)?;
    Ok(off as usize + pixels.len())
}

/// The stages this example drives the surface through, one per run-loop
/// turn (at most one `Host::tick()` per turn keeps [`WinBackend`]'s detach
/// and re-attach in separate turns — the caller property the macOS backend
/// requires for the Flutter engine's one-view-per-engine-at-a-time rule, and
/// which [`WinBackend`]'s docs ask drivers to keep honoring even though the
/// Windows reparent showed no analog of the constraint).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    /// Sample RSS with no surface registered yet, then proceed at once.
    Init,
    /// Open the window, report it to the host, and tick — the host starts
    /// the renderer and attaches the view synchronously (`WinBackend`
    /// always reports [`Readiness::Ready`] for Flutter — W-F4).
    OpenWindow,
    /// Rasterize the now-attached Flutter view to a BMP.
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
    backend: Rc<RefCell<WinBackend>>,
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

    let data_dir = flutter_data_dir();
    println!("[lifecycle] Flutter data dir: {data_dir}");
    let backend = Rc::new(RefCell::new(WinBackend::new(data_dir)));

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
                // just waiting for the Win32 pump to process the previous
                // step, one run-loop turn at a time.
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
            let path = std::env::temp_dir()
                .join("duet_backend_windows_lifecycle.bmp")
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
    println!("=== Duet backend-windows lifecycle RSS report ===");
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
    // The attach sample lands ~0.3s in, before the raster backend and the
    // Dart heap have warmed up — measured on macOS at ~166 MB against a
    // settled ~226 MB. Using it understates the reclaim by roughly 60 MB and
    // answers the wrong question: what matters is how much a *live,
    // rendering* surface costs versus one that has been torn down. The
    // Windows spike's own teardown probe (W-F1) used the same settled
    // baseline, sampled at steady state right before the destroy.
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
    // denominator rather than trusting the sample: a failed memory query
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
            "FlutterDesktopViewControllerDestroy is what reclaims it, not detaching the view",
        ),
    ] {
        println!("{}: {label}", if ok { "PASS" } else { "FAIL" });
    }
    println!();

    assert!(
        enough,
        "teardown must reclaim at least {:.0}% of the engine's own cost; measured {:.1}% \
         ({reclaimed} kB of {engine_cost} kB, suspended={suspended} kB, \
         torn_down={torn_down} kB, start={start} kB) — see FINDINGS.md",
        MIN_RECLAIM_SHARE * 100.0,
        reclaim_share * 100.0
    );
    assert!(
        shutdown_did_it,
        "detaching the view must not be what reclaims memory — the Windows spike (W-F1) \
         established that FlutterDesktopViewControllerDestroy is; detach accounted for {:.1}% of \
         the reclaim ({by_detach} kB of {reclaimed} kB), over the {:.0}% ceiling",
        detach_share * 100.0,
        MAX_DETACH_SHARE * 100.0
    );
}

/// `part / whole` as a share, or 0.0 if `whole` is not positive.
///
/// `rss_kb` returns -1 when the process-memory query failed, and a negative
/// or zero denominator would produce an infinity or a NaN. A NaN compares
/// false against every threshold, so it would report as a failure — correct
/// by accident, but with a message naming a nonsense percentage. Zero fails
/// the floor and passes the ceiling, both of which are the honest answers to
/// "the measurement did not work".
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
