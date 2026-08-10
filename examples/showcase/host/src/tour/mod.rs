//! The scripted tour: two guests, one store, and a teardown in the middle.
//!
//! # Why a script
//!
//! A showcase should be an app, and this one is: both guests have real UIs, real
//! buttons, and behave on their own without being told what to do. But on the
//! machine this was written on there is no reachable on-screen WindowServer for
//! a spawned process, so nobody can watch it. The host therefore *narrates* —
//! it walks a fixed sequence of acts, prints what it did, prints what each guest
//! reports back through the store, and checks the two agree. Everything it
//! checks is a value a guest wrote, not a value the host arranged.
//!
//! # One event loop, two renderers
//!
//! `wry` needs a `tao` event loop and the Flutter engine needs the main
//! thread's run loop; they cannot both be the driver, so the `tao` loop is it
//! and the engine rides alongside. That is the arrangement the two run-loop
//! spikes measured (`spikes/spike-b-macos`, `spikes/spike-b-windows`) and
//! each backend's `examples/two_guests.rs` proved.
//!
//! # Staged, not slept
//!
//! Each turn of the loop advances at most one [`Step`]. Two independent
//! asynchronous renderers cannot be sequenced by sleeping and hoping, and a
//! whole-run [`DEADLINE`] backs the staging up: a hung demo that waits forever
//! is indistinguishable from a slow one, so timing out names the act it stopped
//! at and exits non-zero.

pub mod acts;
pub mod fields;
pub mod guests;
pub mod report;
pub mod rss;

/// The platform backend, under one name.
///
/// The two backend crates export the same API deliberately — same types, same
/// signatures, same teardown contract (`crates/duet-backend-windows` was
/// written to be read side by side with `crates/duet-backend-macos`) — so one
/// alias is all the tour needs to drive either. What genuinely differs
/// (detach parks the view on Windows instead of dropping the controller; what
/// `destroy_renderer` destroys) lives behind the same four `WindowBackend`
/// methods and never reaches this crate.
#[cfg(target_os = "macos")]
pub use duet_backend_macos as backend;
#[cfg(target_os = "windows")]
pub use duet_backend_windows as backend;

/// The backend struct itself — the one exported name the two crates spell
/// differently.
#[cfg(target_os = "macos")]
pub type PlatformBackend = backend::MacBackend;
#[cfg(target_os = "windows")]
pub type PlatformBackend = backend::WinBackend;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use duet::{Value, install};
use duet_core::{Notification, SubscriberId};

use crate::tour::backend::{DuetEvent, FlutterSurface, ProxySink, WebviewSurface};
use duet_runtime::{Runtime, StoreHandle};
use duet_supervisor::SurfaceId;
use tao::dpi::LogicalSize;
use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoop, EventLoopProxy, EventLoopWindowTarget};
use tao::window::{Window, WindowBuilder};

use duet_showcase::state::initial_state;

use crate::tour::fields::Fields;
use crate::tour::guests::{PROBE_JS, WebProbe, parse_probe};
use crate::tour::report::Results;
use crate::tour::rss::Sample;

/// How long the whole tour gets before it is declared hung.
///
/// Generous, because it covers two Flutter engine boots, a WebKit page load, two
/// Dart handshakes and a settle window. A healthy run finishes in well under a
/// third of it.
pub const DEADLINE: Duration = Duration::from_secs(150);

/// Milliseconds between turns. Each turn advances at most one act.
pub const TURN_MS: u64 = 50;

/// Turns to keep polling after the thing being waited for arrives, before
/// checking. Every count here is exact, and a duplicate or late push arriving a
/// moment later would otherwise pass unnoticed.
pub const SETTLE_TURNS: u32 = 20;

/// The least of the Flutter engine's own cost that tearing it down must give
/// back.
///
/// A share rather than a number of kilobytes, for the reason
/// `crates/duet-backend-macos/examples/lifecycle.rs` sets out at length: an
/// absolute floor silently encodes which guest app was booted. The engine's cost
/// here is measured as the growth between "webview guest live" and "both guests
/// live", which also contains whatever the Dart app allocated — so the floor is
/// set well below the ~60 % that example measures for the engine alone.
///
/// A whole percent, in integers: every quantity involved is a kilobyte count
/// from `ps`, and a floating-point comparison would add rounding to a
/// measurement that has none.
pub const MIN_RECLAIM_PERCENT: i64 = 30;

/// The one line the host writes itself, while the Flutter guest is gone.
pub const AWAY_LINE: &str = "Host: written while the Flutter guest did not exist.";

/// Everything the driver needs across turns.
pub struct Tour {
    /// When the tour started, for the deadline and for elapsed-time reporting.
    pub start: Instant,
    /// The act currently in progress.
    pub step: Step,
    /// Turns left in the current settle window.
    pub settle: u32,
    /// `None` once the report has shut it down. `tao`'s loop never returns and
    /// drops nothing, so the core thread must be stopped explicitly.
    pub runtime: Option<Runtime>,
    /// A handle to the same store, for the operations that are not typed fields.
    pub store: StoreHandle,
    /// The host's typed view of the store.
    pub fields: Fields,
    /// The platform backend: Flutter engines and their windows.
    pub backend: PlatformBackend,
    /// The surface the Flutter guest is registered under.
    pub flutter_id: SurfaceId,
    /// `None` while the Flutter guest is torn down — which is the point of acts
    /// 4 through 6.
    pub flutter: Option<FlutterSurface>,
    /// The store identity of the *current* Flutter guest. A new one is allocated
    /// on reboot; the old one is dropped, subscriptions and all.
    pub flutter_sub: SubscriberId,
    /// The webview guest, alive for the whole run.
    pub webview: Option<WebviewSurface>,
    /// Kept only so the webview it hosts stays alive.
    pub web_window: Option<Window>,
    /// The store identity of the webview guest.
    pub web_sub: SubscriberId,
    /// The showcase bundle, evaluated into the webview once it has booted.
    pub bundle: String,
    /// Written by the readback callback (which `wry` requires to be
    /// `Send + 'static`, so it cannot borrow this struct) and read on the next
    /// turn.
    pub probe: Arc<Mutex<WebProbe>>,
    /// The RSS table.
    pub samples: Vec<Sample>,
    /// Every claim, with its evidence.
    pub results: Results,
    /// How long to stay alive after the tour, for hot reload.
    pub linger: Duration,
    /// When lingering ends.
    pub linger_until: Option<Instant>,
    /// How many lines were in the document at the moment of teardown.
    pub lines_at_teardown: usize,
    /// The act the deadline fired at, if it did.
    pub timed_out_at: Option<&'static str>,
    /// What the process will exit with.
    pub exit_code: i32,
    /// The last trouble the webview guest reported, so it is printed on change
    /// rather than on every turn.
    pub web_trouble: String,
    /// The last value of `flutter.note`, for the same reason.
    pub last_note: String,
    /// The last value of `web.saw_peer_note`, for the same reason.
    pub last_peer_note: String,
}

/// The acts, in order. Each is one turn's worth of work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Wait for the `wry` bootstrap page to define `window.__duet`.
    AwaitWebBoot,
    /// Evaluate the showcase bundle into the webview.
    MountWebGuest,
    /// Wait for the bundle to report its panel is up.
    AwaitWebMounted,
    /// Open a window, boot a Flutter engine, attach its view.
    BootFlutter,
    /// Wait until both guests have finished their opening moves.
    AwaitGuestsReady,
    /// Settle, then check acts 1 to 3: shared state, subscriptions, commands.
    CheckOpening,
    /// Stay alive so `duet dev` has something to hot reload.
    Linger,
    /// Sample RSS with both guests live.
    SampleLive,
    /// Tear the Flutter guest down and wipe what it published.
    TearDownFlutter,
    /// Let the allocator settle before measuring.
    SettleTeardown,
    /// Sample RSS again and check that memory came back.
    SampleAfterTeardown,
    /// Append a line while the Flutter guest does not exist.
    WriteWhileAway,
    /// Check the surviving guest noticed it.
    AwaitWebSawIt,
    /// Boot a second Flutter engine for the same surface.
    RebootFlutter,
    /// Wait for the new guest to finish its opening moves.
    AwaitFlutterBack,
    /// Settle, then check that it rediscovered the state it never saw written.
    CheckRestored,
    /// Sample RSS one last time.
    SampleAfterReboot,
    /// Shut everything down, print, exit.
    Report,
}

impl Step {
    /// The act's name, for the timeout report.
    pub fn name(self) -> &'static str {
        match self {
            Step::AwaitWebBoot => "AwaitWebBoot",
            Step::MountWebGuest => "MountWebGuest",
            Step::AwaitWebMounted => "AwaitWebMounted",
            Step::BootFlutter => "BootFlutter",
            Step::AwaitGuestsReady => "AwaitGuestsReady",
            Step::CheckOpening => "CheckOpening",
            Step::SampleLive => "SampleLive",
            Step::TearDownFlutter => "TearDownFlutter",
            Step::SettleTeardown => "SettleTeardown",
            Step::SampleAfterTeardown => "SampleAfterTeardown",
            Step::WriteWhileAway => "WriteWhileAway",
            Step::AwaitWebSawIt => "AwaitWebSawIt",
            Step::RebootFlutter => "RebootFlutter",
            Step::AwaitFlutterBack => "AwaitFlutterBack",
            Step::CheckRestored => "CheckRestored",
            Step::SampleAfterReboot => "SampleAfterReboot",
            Step::Linger => "Linger",
            Step::Report => "Report",
        }
    }
}

/// Builds the store, the backend and the tour. No window and no guest yet:
/// both need the event loop's window target.
///
/// # Errors
///
/// Returns a human-readable reason if the store will not seed.
pub fn setup(
    event_loop: &EventLoop<DuetEvent>,
    framework: &str,
    bundle: String,
    linger: Duration,
) -> Result<Tour, String> {
    let proxy = event_loop.create_proxy();
    let runtime = Runtime::spawn(Value::Null, ProxySink::new(proxy));

    // The store starts as the Rust definition says it should. `install` writes
    // the whole tree at the root after validating it against its own schema, so
    // every path a generated accessor names exists before any guest asks.
    let typed = install(runtime.handle(), &initial_state())
        .map_err(|e| format!("installing the showcase state failed: {e}"))?;
    let fields = Fields::bind(&typed).map_err(|e| format!("a field path is not a path: {e}"))?;

    // Two subscribers from one runtime. This is the only thing that
    // distinguishes the guests to the store, and neither guest can name one —
    // `duet_protocol::Request` has no subscriber field — so they are the host's
    // alone.
    let web_sub = runtime.next_subscriber_id();
    let flutter_sub = runtime.next_subscriber_id();

    Ok(Tour {
        start: Instant::now(),
        step: Step::AwaitWebBoot,
        settle: 0,
        store: runtime.handle(),
        runtime: Some(runtime),
        fields,
        backend: PlatformBackend::new(framework),
        flutter_id: SurfaceId::from_raw(1),
        flutter: None,
        flutter_sub,
        webview: None,
        web_window: None,
        web_sub,
        bundle,
        probe: Arc::new(Mutex::new(WebProbe::default())),
        samples: vec![Sample::take("before either guest exists")],
        results: Results::default(),
        linger,
        linger_until: None,
        lines_at_teardown: 0,
        timed_out_at: None,
        exit_code: 0,
        web_trouble: String::new(),
        last_note: String::new(),
        last_peer_note: String::new(),
    })
}

impl Tour {
    /// Creates the webview guest's window and surface.
    ///
    /// # Errors
    ///
    /// Returns a human-readable reason if `tao` or `wry` refuses.
    pub fn open_webview(
        &mut self,
        target: &EventLoopWindowTarget<DuetEvent>,
        proxy: EventLoopProxy<DuetEvent>,
    ) -> Result<(), String> {
        let window = WindowBuilder::new()
            .with_title("Duet showcase — WebView guest")
            .with_inner_size(LogicalSize::new(560.0, 640.0))
            .with_position(tao::dpi::LogicalPosition::new(660.0, 80.0))
            .build(target)
            .map_err(|e| format!("creating the webview's window failed: {e}"))?;
        let surface = WebviewSurface::with_commands(
            &window,
            self.store.clone(),
            self.web_sub,
            proxy,
            &duet_showcase::commands::COMMANDS,
        )
        .map_err(|e| format!("creating the wry webview failed: {e}"))?;
        self.web_window = Some(window);
        self.webview = Some(surface);
        Ok(())
    }

    /// Seconds since the tour started, for progress lines.
    pub fn elapsed(&self) -> f64 {
        self.start.elapsed().as_secs_f64()
    }

    /// Counts the settle window down, returning whether it has elapsed.
    pub fn settled(&mut self) -> bool {
        self.settle = self.settle.saturating_sub(1);
        self.settle == 0
    }
}

/// The event-loop body.
pub fn drive(
    tour: &mut Tour,
    event: Event<DuetEvent>,
    target: &EventLoopWindowTarget<DuetEvent>,
    proxy: &EventLoopProxy<DuetEvent>,
    control_flow: &mut ControlFlow,
) {
    match event {
        Event::NewEvents(StartCause::Init) => {
            if let Err(reason) = tour.open_webview(target, proxy.clone()) {
                println!("FAIL: setup — {reason}");
                tour.exit_code = 1;
                tour.step = Step::Report;
            }
            *control_flow = next_turn();
        }
        Event::NewEvents(StartCause::ResumeTimeReached { .. }) => {
            // `advance` never returns once it reaches the report: that act
            // prints and exits the process, because `tao`'s loop never returns
            // and would otherwise leave the exit code to the platform.
            advance(tour, target);
            *control_flow = next_turn();
        }
        // The webview's reply path. `deliver` rather than `eval`: the event names
        // the surface its reply was produced for, and the surface checks. With
        // two guests in this process, a driver that evaluated every script into
        // whichever webview it happened to hold would settle the wrong pending
        // call and hand one renderer another renderer's data.
        Event::UserEvent(DuetEvent::WebviewScript { subscriber, script }) => {
            match tour.webview.as_ref() {
                Some(surface) => {
                    if let Err(e) = surface.deliver(subscriber, &script) {
                        println!("[showcase] delivering a reply to the webview failed: {e}");
                    }
                }
                None => println!("[showcase] dropped a reply for the torn-down webview"),
            }
        }
        Event::UserEvent(DuetEvent::Notifications(batch)) => deliver_pushes(tour, batch),
        Event::UserEvent(DuetEvent::Tick) => {}
        _ => {}
    }
}

/// Schedules the next turn.
fn next_turn() -> ControlFlow {
    ControlFlow::WaitUntil(std::time::Instant::now() + Duration::from_millis(TURN_MS))
}

/// Hands **every** notification to **both** surfaces.
///
/// The fan-out is deliberate. Each surface's `push` compares the notification's
/// subscriber against its own and drops anything addressed elsewhere, so routing
/// the whole stream through both exercises that boundary instead of sidestepping
/// it with a check at the call site. A guest cannot be handed another guest's
/// state by a host that forgot an `if`.
fn deliver_pushes(tour: &mut Tour, batch: Vec<Notification>) {
    for note in batch {
        if let Some(surface) = tour.webview.as_ref() {
            if let Err(e) = surface.push(&note) {
                println!("[showcase] the webview surface refused a notification: {e}");
            }
        }
        if let Some(surface) = tour.flutter.as_ref() {
            if let Err(e) = surface.push(&note) {
                println!("[showcase] the Flutter surface refused a notification: {e}");
            }
        }
    }
}

/// Advances by at most one act, then asks the webview for a fresh readback for
/// the next turn to act on.
fn advance(tour: &mut Tour, target: &EventLoopWindowTarget<DuetEvent>) {
    let exempt = matches!(tour.step, Step::Linger | Step::Report);
    if !exempt && tour.start.elapsed() > DEADLINE {
        tour.timed_out_at = Some(tour.step.name());
        tour.step = Step::Report;
    }

    let probe = read_probe(tour);
    match tour.step {
        Step::AwaitWebBoot => acts::await_web_boot(tour, &probe),
        Step::MountWebGuest => acts::mount_web_guest(tour),
        Step::AwaitWebMounted => acts::await_web_mounted(tour, &probe),
        Step::BootFlutter => acts::boot_flutter(tour, target),
        Step::AwaitGuestsReady => acts::await_guests_ready(tour),
        Step::CheckOpening => acts::check_opening(tour),
        Step::SampleLive => acts::sample_live(tour),
        Step::TearDownFlutter => acts::tear_down_flutter(tour),
        Step::SettleTeardown => acts::settle_teardown(tour),
        Step::SampleAfterTeardown => acts::sample_after_teardown(tour),
        Step::WriteWhileAway => acts::write_while_away(tour),
        Step::AwaitWebSawIt => acts::await_web_saw_it(tour),
        Step::RebootFlutter => acts::reboot_flutter(tour, target),
        Step::AwaitFlutterBack => acts::await_flutter_back(tour),
        Step::CheckRestored => acts::check_restored(tour),
        Step::SampleAfterReboot => acts::sample_after_reboot(tour),
        Step::Linger => acts::linger(tour),
        Step::Report => acts::report(tour),
    }
    request_probe(tour);
}

/// Copies the latest readback out from under its lock.
///
/// A poisoned lock means the callback panicked; that is treated as "no data
/// yet", so the deadline reports it rather than this turn panicking too.
fn read_probe(tour: &Tour) -> WebProbe {
    match tour.probe.lock() {
        Ok(guard) => guard.clone(),
        Err(_) => WebProbe::default(),
    }
}

/// Asks the webview guest for a fresh readback; the callback lands on a later
/// turn.
fn request_probe(tour: &Tour) {
    let Some(surface) = tour.webview.as_ref() else {
        return;
    };
    let slot = Arc::clone(&tour.probe);
    let outcome = surface.eval_with_callback(PROBE_JS, move |json| {
        if let Ok(mut guard) = slot.lock() {
            *guard = parse_probe(&json);
        }
    });
    if let Err(e) = outcome {
        println!("[showcase] the webview readback failed: {e}");
    }
}
