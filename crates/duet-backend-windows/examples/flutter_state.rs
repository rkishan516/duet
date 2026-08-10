//! The shared-state proof for the Flutter guest: a real Dart isolate running
//! inside a real `FlutterEngine` writes into the store over a real platform
//! channel, Rust reads the value back, and then Rust's own write travels the
//! other way as a push the guest receives.
//!
//! The sibling of `examples/webview_state.rs`, which proves the same claims
//! for a JavaScript guest over `wry` IPC. Everything under `duet-protocol` is
//! tested without a guest: its unit tests call [`duet_protocol::handle_text`]
//! directly, which proves the *routing* but says nothing about whether
//! `FlutterDesktopMessengerSetCallback` ever hands a Dart
//! `BasicMessageChannel.send` to that function, or whether a
//! `FlutterDesktopMessengerSend` reaches the guest's own handler. This example
//! closes both halves of that loop over the real transport.
//!
//! # Build and run
//!
//! The guest is a tracked fixture and must be compiled to a Windows Flutter
//! bundle first — a debug/JIT build, which is the only kind this project has
//! ever produced (release/AOT behaviour here is unverified):
//!
//! ```text
//! (cd fixtures/duet_guest && flutter build windows --debug)
//! DUET_FLUTTER_BUNDLE=fixtures/duet_guest/build/windows/x64/runner/Debug/data \
//!   cargo run -p duet-backend-windows --example flutter_state
//! ```
//!
//! `DUET_FLUTTER_BUNDLE` defaults to that same path, so the env var is only
//! needed when running from another directory.
//!
//! # What this proves, and what it cannot prove here
//!
//! Provable here: that a Dart isolate runs, that
//! `FlutterDesktopMessengerSetCallback` delivers guest messages to Rust, that
//! `FlutterDesktopMessengerSend` delivers host pushes to the guest, and that
//! the store is genuinely shared between the two. All of that is in-process
//! and observable from Rust.
//!
//! **Not** provable here, and not claimed: that real human input works. On
//! macOS the unprovable thing was visibility itself — no reachable on-screen
//! WindowServer — but this machine has a real display session, and windows
//! genuinely appear on a physical display when an example opens one. This
//! example opens none: it is deliberately headless — no `tao` window, no view
//! controller — because none of the claims need one
//! (`spikes/spike-b-windows/FINDINGS.md` W-F3: the engine boots and runs Dart
//! with no controller at all), and a controller-less engine never starts frame
//! scheduling, so the macOS F1 backing-store concern has no purchase here.
//! What stays unverified on this machine is real mouse and keyboard input:
//! every run is autonomous, with nobody at the keyboard, and W-F7 shows posted
//! synthetic input reaching Flutter but not WebView2 — WndProc delivery, not
//! full routing.
//!
//! # How the guest reports back: through the store, not around it
//!
//! There is no side channel. `fixtures/duet_guest/lib/main.dart` publishes
//! everything it observes — its push count, the host's replies to hostile
//! input, the exact doubles it wrote — as one map at [`REPORT_PATH`], written
//! over the very channel under test, and this driver polls that path out of
//! the shared store. A report that arrives at all is therefore already
//! evidence the guest → host direction works, and no claim here depends on a
//! transport the proof does not already trust. The `stage` field in that map
//! is what sequences the two sides (`ready` → `probed` → `armed` → `done`).
//!
//! # The genuinely new ground: an `f64` over a platform channel
//!
//! No `f64` had ever crossed a Flutter platform channel in this project before
//! this file. Every prior spike sent only small strings and integer counters
//! (ping replies, frame markers), so `serde_json`'s `float_roundtrip` feature
//! — mandatory in `duet-codec`, `duet-protocol` and this crate's
//! dev-dependencies — had **zero** coverage on this transport. Dart writes
//! each double as a JSON number produced by `double.toString()` (shortest
//! round-trip decimal) and this driver compares [`f64::to_bits`], never `==`:
//! `0.1 + 0.2` and a subnormal are exactly the inputs a parser without a
//! correctly-rounding slow path gets *nearly* right, and a `==` comparison
//! that happened to pass on one value would hide it on the next.
//!
//! # Hostile input over the real channel
//!
//! `duet-protocol`'s unit tests already drive malformed text through
//! `handle_text` directly. Nothing until now drove it through an actual
//! platform channel, where the payload also passes
//! [`duet_backend_windows::FlutterSurface`]'s own inbound size cap and its
//! `catch_unwind` barrier before reaching the router. The 1 MB
//! parseable-but-unresolvable path is a regression guard specifically: a
//! recently-fixed bug echoed the guest's whole path back into the failure
//! message, producing a 1,000,109-byte reply.
//!
//! # Staged, not raced
//!
//! Like `examples/webview_state.rs`, this drives a [`Step`] enum one stage per
//! run-loop turn. The sequence is asynchronous in both directions — a request
//! crosses to Rust and its reply crosses back on a later turn — so staging is
//! what makes it assertable rather than flaky. A whole-run [`DEADLINE`] backs
//! that up: a hung example is indistinguishable from a slow one and would
//! wedge CI, so timing out reports *which stage* it stopped at, prints the
//! last guest state it saw, and exits non-zero rather than waiting forever.
//!
//! # No `tao` event loop
//!
//! `examples/lifecycle.rs` and `examples/webview_state.rs` both need one — the
//! first to own windows, the second because `wry`'s IPC replies are marshalled
//! through an `EventLoopProxy`. Neither applies here: a Flutter platform
//! channel handler is invoked inline on the platform thread (W-F5) and replies
//! inline too, and the engine processes its platform tasks "transparently
//! through DispatchMessage" — its task runner posts them to a hidden window on
//! this thread — so all this driver needs is to keep pumping this thread's
//! message queue. A plain `PeekMessageW`/`DispatchMessageW` loop does that
//! directly, which keeps the turn structure of this file explicit instead of
//! hidden inside `tao`'s `ControlFlow`.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use duet_backend_windows::{FlutterSurface, WinBackend};
use duet_core::{Notification, Path, SubscriberId, Value};
use duet_host::WindowBackend;
use duet_protocol::{Response, decode_response};
use duet_runtime::{Runtime, Sink, SinkError, StoreHandle};
use duet_supervisor::SurfaceId;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, MSG, MsgWaitForMultipleObjects, PM_REMOVE, PeekMessageW, QS_ALLINPUT,
    TranslateMessage,
};

/// How long the whole sequence gets before it is declared hung.
///
/// Generous: the work here is an engine boot (`FlutterDesktopEngineRun`
/// returns with the isolate already serving — W-F4), a handshake, a dozen
/// channel round trips — two of which carry a megabyte — and the guest's own
/// observation window. All of it lands in well under 20 seconds when it lands
/// at all.
const DEADLINE: Duration = Duration::from_secs(60);

/// Seconds of run loop per turn. Each turn advances at most one stage.
const TURN_SECS: f64 = 0.05;

/// Turns to keep polling *after* the first push arrives, before asserting the
/// count. The claim is "exactly one push", and a duplicate arriving a moment
/// later would otherwise pass unnoticed. Twenty turns is a full second — far
/// longer than the guest needs to notice a push and republish its count.
const SETTLE_TURNS: u32 = 20;

/// Where the Dart guest publishes everything it observes. Must match
/// `kReportPath` in `fixtures/duet_guest/lib/main.dart`.
const REPORT_PATH: &str = "guest";

/// The path both sides write and watch. Dart writes 42 here; Rust writes 99.
const COUNTER_PATH: &str = "counter";

/// The largest a `failed` reply may be before this example calls it an
/// unbounded echo, in bytes.
///
/// Same value and same reasoning as `MAX_REASONABLE_REPLY` in
/// `duet-protocol`'s own hostile-input test: the reply to a rejected request
/// is a short, fixed-shape JSON object and must never scale with the size of
/// the offending input. Generous headroom over the ~100-byte replies this
/// actually produces — the point is "bounded", not a tight bound. The bug this
/// guards against produced 1,000,109 bytes.
const MAX_REASONABLE_REPLY: i64 = 4096;

/// The doubles the guest writes, paired with the exact `f64` this side
/// expects. Names must match `_doubleProbes()` in the Dart fixture.
///
/// Each is chosen because its shortest decimal form is long, extreme, or both.
/// `0.1 + 0.2` is the classic 17-significant-digit case; `f64::MAX` and the
/// smallest subnormal sit at the two ends of the exponent range, and the
/// subnormal's decimal form (`5e-324`) has no exact binary counterpart at all;
/// `-0.0` checks a sign bit with no magnitude survives, which naive handling
/// silently loses. Rust's own const evaluation of `0.1 + 0.2` and `1.0 / 3.0`
/// uses IEEE-754 semantics, so these are the same bit patterns Dart computes.
fn float_probes() -> [(&'static str, f64); 5] {
    [
        ("sum", 0.1 + 0.2),
        ("third", 1.0 / 3.0),
        ("maxFinite", f64::MAX),
        // The smallest positive subnormal — `double.minPositive` in Dart.
        // Written as a bit pattern rather than `5e-324` so this expectation
        // cannot itself be a victim of the parser under test.
        ("minPositive", f64::from_bits(1)),
        ("negativeZero", -0.0),
    ]
}

/// The hostile payloads the guest sends raw, and what the reply's echoed id
/// must be. Names must match `_probeHostile()` in the Dart fixture.
///
/// The id is the discriminator that says *where* a payload was refused. A
/// request the router decoded far enough to read an id echoes it back;
/// anything refused before that — malformed JSON, or a payload over the
/// surface's inbound cap — answers with id 0, because reading the real id
/// would mean parsing the very text being refused.
fn hostile_probes() -> [(&'static str, u64); 5] {
    [
        ("empty", 0),
        ("notJson", 0),
        ("emptyObject", 0),
        // Decoded far enough to read `"id":"9"`, then refused while resolving
        // a 1 MB path. The echoed id proves it reached the router.
        ("megabytePath", 9),
        // Refused by `FlutterSurface`'s inbound cap before any decoding.
        ("overCap", 0),
    ]
}

/// One assertion's outcome: `None` means the run never got that far.
type Outcome = Option<(bool, String)>;

/// Every claim this example makes, in report order.
#[derive(Debug, Default)]
struct Results {
    /// The Dart guest booted and reached the host over `duet/rpc`.
    guest_reached_host: Outcome,
    /// The headline: a Dart-written value is readable from Rust, exactly.
    rust_reads_dart_write: Outcome,
    /// Every probe double survived the channel bit-for-bit.
    floats_round_trip: Outcome,
    /// Every hostile payload was answered with a bounded `failed` reply.
    hostile_bounded: Outcome,
    /// A Rust write reached the guest as exactly one push.
    push_arrived_once: Outcome,
    /// That push carried 99, at `counter`.
    push_carried_value: Outcome,
}

/// The stages this example drives, one per run-loop turn. See the module docs
/// on why staging (rather than sleeping and hoping) is what makes an
/// asynchronous two-directional sequence assertable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    /// Poll until the guest has published any report at all.
    AwaitGuest,
    /// Poll until the guest reports `stage == "armed"`, i.e. it has written
    /// `counter`, recorded its probes, and subscribed.
    AwaitArmed,
    /// Read `counter` from Rust and require exactly `Value::Int(42)`.
    AssertDartWrite,
    /// Compare every probe double's bit pattern.
    AssertFloats,
    /// Decode every hostile reply and bound its length.
    AssertHostile,
    /// Write `Value::Int(99)` from Rust, which should notify the guest.
    RustWrite,
    /// Poll until the guest reports its first push.
    AwaitPush,
    /// Keep polling a little longer, so a *second* push cannot slip past the
    /// "exactly one" claim.
    SettlePush,
    /// Print the report and stop.
    Report,
}

/// Notifications land on the runtime's core thread; this stashes them for the
/// platform thread to drain and push.
///
/// `ProxySink` (this crate's real sink) marshals onto a `tao` event loop,
/// which this headless example deliberately does not have — see the module
/// docs. A mutex-backed queue is the equivalent for a driver that owns its own
/// run loop, and it honours [`Sink::deliver`]'s "must not block, must not call
/// back into the store" contract the same way: extend a `Vec` and return.
#[derive(Clone)]
struct StashSink(Arc<Mutex<Vec<Notification>>>);

impl StashSink {
    fn new() -> Self {
        StashSink(Arc::new(Mutex::new(Vec::new())))
    }

    /// Takes everything queued so far. A poisoned lock (a panic inside
    /// `deliver`) yields an empty batch rather than propagating: the deadline
    /// is what should report a stalled run, not a panic from the drain site.
    fn drain(&self) -> Vec<Notification> {
        match self.0.lock() {
            Ok(mut queue) => queue.drain(..).collect(),
            Err(_) => Vec::new(),
        }
    }
}

impl Sink for StashSink {
    fn deliver(&self, batch: Vec<Notification>) -> Result<(), SinkError> {
        match self.0.lock() {
            Ok(mut queue) => {
                queue.extend(batch);
                Ok(())
            }
            // A poisoned lock means this queue can no longer be trusted, which
            // is exactly what `Closed` describes to the core thread: stop
            // delivering here, keep serving the store.
            Err(_) => Err(SinkError::Closed),
        }
    }
}

/// Everything the driver needs across turns of the run loop.
struct App {
    start: Instant,
    step: Step,
    store: StoreHandle,
    subscriber: SubscriberId,
    counter: Path,
    report: Path,
    /// The guest's last published report, for assertions and for the failure
    /// printout.
    last_report: Option<Value>,
    results: Results,
    settle_left: u32,
    /// Notifications this surface's subscriber received from the store.
    notifications_seen: usize,
    /// Of those, how many were successfully sent to the guest.
    pushes_delivered: usize,
    /// The stage the deadline fired at, if it did.
    timed_out_at: Option<Step>,
}

/// Path to the Flutter `data` directory produced by `flutter build windows`
/// (`flutter_assets/` + `icudtl.dat`, next to the runner exe).
/// Overridable via env var for other machines, matching
/// `examples/lifecycle.rs`.
fn flutter_data_dir() -> String {
    std::env::var("DUET_FLUTTER_BUNDLE")
        .unwrap_or_else(|_| "fixtures/duet_guest/build/windows/x64/runner/Debug/data".to_string())
}

/// The store both guests share. `counter` is what Dart and Rust take turns
/// writing; `guest` is the single key the Dart driver republishes its whole
/// report into.
///
/// `guest` is seeded rather than left absent purely for legibility — `set`
/// inserts an absent *final* map key, so the guest could create it — but
/// nothing deeper may be seeded away: `set` never creates intermediate nodes,
/// which is exactly why the Dart side writes one whole map instead of
/// `guest.pushes`, `guest.doubles.sum`, and so on.
fn initial_root() -> Value {
    Value::map([(COUNTER_PATH, Value::Int(0)), (REPORT_PATH, Value::Null)])
}

fn main() {
    let data_dir = flutter_data_dir();
    println!("[flutter_state] Flutter data dir: {data_dir}");

    let stash = StashSink::new();
    let runtime = Runtime::spawn(initial_root(), stash.clone());
    let subscriber = runtime.next_subscriber_id();

    let counter = match Path::parse(COUNTER_PATH) {
        Ok(p) => p,
        Err(e) => fail_setup(
            runtime,
            &format!("{COUNTER_PATH:?} must be a valid path: {e:?}"),
        ),
    };
    let report = match Path::parse(REPORT_PATH) {
        Ok(p) => p,
        Err(e) => fail_setup(
            runtime,
            &format!("{REPORT_PATH:?} must be a valid path: {e:?}"),
        ),
    };

    let surface_id = SurfaceId::from_raw(1);
    let mut backend = WinBackend::new(&data_dir);
    // Headless: `start_renderer` boots the engine with no view controller —
    // no `allowHeadlessExecution` analog is needed, the C API simply composes
    // that way (W-F3) — and nothing here ever calls `attach_view`, so no
    // window is created. See the module docs on why that is deliberate.
    if let Err(e) = backend.start_renderer(surface_id) {
        fail_setup(runtime, &format!("could not boot a Flutter engine: {e}"));
    }
    let Some(engine) = backend.engine(surface_id) else {
        fail_setup(
            runtime,
            "the backend reported a booted renderer but has no engine for it",
        );
    };
    let surface = match FlutterSurface::new(engine, runtime.handle(), subscriber) {
        Ok(s) => s,
        Err(e) => fail_setup(
            runtime,
            &format!("could not register the duet/rpc handler: {e}"),
        ),
    };
    println!(
        "[flutter_state] engine booted headless; duet/rpc registered; subscriber={subscriber:?}"
    );

    let mut app = App {
        start: Instant::now(),
        step: Step::AwaitGuest,
        store: runtime.handle(),
        subscriber,
        counter,
        report,
        last_report: None,
        results: Results::default(),
        settle_left: 0,
        notifications_seen: 0,
        pushes_delivered: 0,
        timed_out_at: None,
    };

    while app.step != Step::Report {
        pump_run_loop(TURN_SECS);
        deliver_pushes(&mut app, &surface, &stash);
        app.last_report = read_report(&app);
        if app.start.elapsed() > DEADLINE {
            app.timed_out_at = Some(app.step);
            break;
        }
        advance(&mut app);
    }

    let failures = print_report(&app);

    // Teardown order is not interchangeable: the surface must unregister its
    // channel handler (its `Drop`) before the engine shuts down, because the
    // engine holds the callback's `user_data` pointer past registration — the
    // C analog of the handler map macOS's `shutDownEngine` never cleared; see
    // `FlutterSurface`'s `Drop`. `destroy_renderer` is then what actually
    // reclaims the engine's memory: `FlutterDesktopViewControllerDestroy`
    // owns the engine (W-F1), and with no controller ever created here it is
    // `FlutterDesktopEngineDestroy` that runs.
    drop(surface);
    if let Err(e) = backend.destroy_renderer(surface_id) {
        println!("[flutter_state] destroying the renderer failed: {e}");
    }
    if let Err(e) = runtime.shutdown() {
        println!("[flutter_state] runtime shutdown failed: {e}");
    }

    if failures == 0 {
        println!(
            "ALL PASS: a Dart guest and Rust share one store over a real Flutter platform channel"
        );
        std::process::exit(0);
    }
    println!("{failures} check(s) FAILED");
    std::process::exit(1);
}

/// Reports a setup failure as a FAIL line and exits non-zero, rather than
/// panicking: a machine that cannot boot a Flutter engine or register a
/// channel handler is a finding about the environment, and it should read the
/// same way as any other failure in this example's output.
fn fail_setup(runtime: Runtime, reason: &str) -> ! {
    println!("FAIL: setup - {reason}");
    if let Err(e) = runtime.shutdown() {
        println!("[flutter_state] runtime shutdown failed: {e}");
    }
    std::process::exit(1);
}

/// Runs the platform thread's message pump for `secs`.
///
/// This is what gives the Flutter engine its thread back: the engine
/// processes its platform tasks "transparently through DispatchMessage" — its
/// task runner posts them to a hidden window on this thread — so the Dart
/// isolate's microtasks, its timers, and every inbound platform message are
/// all serviced by pumping this thread's message queue. Without it the guest
/// never runs and nothing in this file can happen.
///
/// Unlike the macOS sibling — where `NSRunLoop::runUntilDate` is a safe
/// binding and the whole example contains no `unsafe` at all — pumping a
/// Win32 message queue means raw `PeekMessageW`/`DispatchMessageW` calls. The
/// two `unsafe` blocks below are this example's only ones, and they are the
/// plain thread message pump every Win32 program runs; everything else
/// reaches the Win32 world only through [`WinBackend`] and [`FlutterSurface`],
/// which own that unsafety.
fn pump_run_loop(secs: f64) {
    let deadline = Instant::now() + Duration::from_secs_f64(secs);
    loop {
        // SAFETY: plain thread message pump on the calling thread; MSG is POD.
        unsafe {
            let mut msg: MSG = std::mem::zeroed();
            while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        let wait_ms = deadline
            .duration_since(now)
            .as_millis()
            .min(u128::from(u32::MAX)) as u32;
        // SAFETY: zero handles; wakes on any queued input or timeout.
        unsafe { MsgWaitForMultipleObjects(0, std::ptr::null(), 0, wait_ms, QS_ALLINPUT) };
    }
}

/// Turns store notifications into guest pushes. Filters on this surface's own
/// subscriber: a notification for another guest must never be sent to this
/// one. [`FlutterSurface::push`] enforces the same boundary itself; doing it
/// here too is what makes `notifications_seen` mean "addressed to us".
fn deliver_pushes(app: &mut App, surface: &FlutterSurface, stash: &StashSink) {
    for note in stash.drain() {
        if note.subscriber != app.subscriber {
            continue;
        }
        app.notifications_seen += 1;
        match surface.push(&note) {
            Ok(()) => app.pushes_delivered += 1,
            Err(e) => println!("[flutter_state] pushing a notification failed: {e}"),
        }
    }
}

/// Reads the guest's published report out of the shared store.
fn read_report(app: &App) -> Option<Value> {
    match app.store.get(&app.report) {
        Ok(value) => value,
        Err(e) => {
            println!("[flutter_state] reading the guest report failed: {e}");
            None
        }
    }
}

/// Advances by at most one stage.
fn advance(app: &mut App) {
    match app.step {
        Step::AwaitGuest => await_guest(app),
        Step::AwaitArmed => await_armed(app),
        Step::AssertDartWrite => assert_dart_write(app),
        Step::AssertFloats => assert_floats(app),
        Step::AssertHostile => assert_hostile(app),
        Step::RustWrite => rust_write(app),
        Step::AwaitPush => await_push(app),
        Step::SettlePush => settle_push(app),
        Step::Report => {}
    }
}

/// The guest's report as a map field lookup, or `None` if it has not published
/// a map yet.
fn field<'a>(value: Option<&'a Value>, key: &str) -> Option<&'a Value> {
    match value? {
        Value::Map(entries) => entries.get(key),
        _ => None,
    }
}

/// The guest's current `stage`, if it has published one.
fn stage(app: &App) -> Option<&str> {
    match field(app.last_report.as_ref(), "stage")? {
        Value::Str(s) => Some(s.as_str()),
        _ => None,
    }
}

/// Waits for the guest to publish anything at all. That first report is
/// itself the proof the Dart isolate booted, found the channel, and reached
/// the host's handler.
fn await_guest(app: &mut App) {
    let Some(stage) = stage(app) else {
        return;
    };
    let detail = format!(
        "the Dart guest published stage {stage:?} at {REPORT_PATH:?} over duet/rpc \
         after {:.2}s",
        app.start.elapsed().as_secs_f64()
    );
    println!("[flutter_state] {detail}");
    app.results.guest_reached_host = Some((true, detail));
    app.step = Step::AwaitArmed;
}

/// Waits until the guest has finished its own probes and subscribed. Any stage
/// past `armed` counts: the guest moves on by itself, and a driver that only
/// accepted the exact instant would race it.
fn await_armed(app: &mut App) {
    let armed = matches!(stage(app), Some("armed") | Some("done"));
    if !armed {
        return;
    }
    println!(
        "[flutter_state] guest armed after {:.2}s (subscription={:?})",
        app.start.elapsed().as_secs_f64(),
        field(app.last_report.as_ref(), "subscription")
    );
    app.step = Step::AssertDartWrite;
}

/// The headline assertion: read `counter` from Rust and require exactly
/// `Value::Int(42)`.
fn assert_dart_write(app: &mut App) {
    let outcome = match app.store.get(&app.counter) {
        Ok(Some(Value::Int(42))) => (
            true,
            "Rust read counter = Int(42), written by Dart over a Flutter platform channel"
                .to_string(),
        ),
        Ok(other) => (
            false,
            format!("Rust read counter = {other:?}, expected Some(Int(42))"),
        ),
        Err(e) => (false, format!("reading counter from Rust failed: {e}")),
    };
    println!("[flutter_state] {}", outcome.1);
    app.results.rust_reads_dart_write = Some(outcome);
    app.step = Step::AssertFloats;
}

/// Compares every probe double bit-for-bit, printing the actual patterns.
///
/// `to_bits`, never `==`: the failure mode this exists to catch is a parser
/// that lands one ULP away, which `==` reports as unequal but a
/// `(a - b).abs() < eps` style check would wave through, and `-0.0 == 0.0`
/// which `==` reports as *equal* despite a different bit pattern.
fn assert_floats(app: &mut App) {
    let mut failures = Vec::new();
    for (name, want) in float_probes() {
        let doubles = field(app.last_report.as_ref(), "doubles");
        let got = match field(doubles, name) {
            Some(Value::Float(f)) => *f,
            other => {
                println!("FAIL: f64 {name} - the guest published {other:?}, expected a Float");
                failures.push(format!("{name}: not a Float ({other:?})"));
                continue;
            }
        };
        let same = got.to_bits() == want.to_bits();
        println!(
            "{}: f64 {name} - guest 0x{:016x} ({got:e}) vs host 0x{:016x} ({want:e})",
            if same { "PASS" } else { "FAIL" },
            got.to_bits(),
            want.to_bits(),
        );
        if !same {
            failures.push(format!(
                "{name}: got 0x{:016x}, want 0x{:016x}",
                got.to_bits(),
                want.to_bits()
            ));
        }
    }
    app.results.floats_round_trip = Some(if failures.is_empty() {
        (
            true,
            format!(
                "all {} probe doubles round-tripped bit-exactly",
                float_probes().len()
            ),
        )
    } else {
        (false, format!("bit mismatches: {}", failures.join("; ")))
    });
    app.step = Step::AssertHostile;
}

/// Decodes every hostile reply the guest recorded and bounds its length.
fn assert_hostile(app: &mut App) {
    let mut failures = Vec::new();
    for (name, want_id) in hostile_probes() {
        match check_one_hostile(app, name, want_id) {
            Ok(detail) => println!("PASS: hostile {name} - {detail}"),
            Err(detail) => {
                println!("FAIL: hostile {name} - {detail}");
                failures.push(format!("{name}: {detail}"));
            }
        }
    }
    app.results.hostile_bounded = Some(if failures.is_empty() {
        (
            true,
            format!(
                "all {} hostile payloads were answered with bounded failed replies",
                hostile_probes().len()
            ),
        )
    } else {
        (false, failures.join("; "))
    });
    app.step = Step::RustWrite;
}

/// Checks one hostile exchange, returning a detail line either way.
fn check_one_hostile(app: &App, name: &str, want_id: u64) -> Result<String, String> {
    let cases = field(app.last_report.as_ref(), "hostile");
    let case = field(cases, name);
    let Some(Value::Int(request_bytes)) = field(case, "requestBytes") else {
        return Err("the guest published no requestBytes for this case".to_string());
    };
    let Some(Value::Int(reply_bytes)) = field(case, "replyBytes") else {
        return Err("the guest published no replyBytes for this case".to_string());
    };
    let Some(Value::Str(reply)) = field(case, "reply") else {
        return Err("the guest published no reply text for this case".to_string());
    };
    let sizes = format!("{request_bytes} bytes in -> {reply_bytes} bytes out");

    if *reply_bytes < 0 {
        return Err(format!("{sizes}: the host never answered"));
    }
    if *reply_bytes >= MAX_REASONABLE_REPLY {
        return Err(format!(
            "{sizes}: reply is not bounded (limit {MAX_REASONABLE_REPLY} bytes) - \
             the host is echoing guest text back"
        ));
    }
    let json: serde_json::Value = match serde_json::from_str(reply) {
        Ok(j) => j,
        Err(e) => return Err(format!("{sizes}: reply is not valid JSON: {e}")),
    };
    match decode_response(&json) {
        Ok(Response::Failed { id, message }) if id.0 == want_id => {
            Ok(format!("{sizes}: Failed(id={}) {message:?}", id.0))
        }
        Ok(Response::Failed { id, .. }) => Err(format!(
            "{sizes}: Failed but echoed id {}, expected {want_id}",
            id.0
        )),
        Ok(other) => Err(format!(
            "{sizes}: expected a Failed response, got {other:?}"
        )),
        Err(e) => Err(format!("{sizes}: reply does not decode as a Response: {e}")),
    }
}

/// Writes 99 from Rust. The store notifies this surface's subscriber, which
/// [`StashSink`] queues for [`deliver_pushes`] to send on the next turn.
fn rust_write(app: &mut App) {
    match app.store.set(&app.counter, Value::Int(99)) {
        Ok(()) => println!("[flutter_state] Rust wrote counter = Int(99)"),
        Err(e) => println!("[flutter_state] Rust write of counter failed: {e}"),
    }
    app.step = Step::AwaitPush;
}

/// Waits for the guest to report its first push, then holds for
/// [`SETTLE_TURNS`] more so a duplicate cannot pass the "exactly one" claim.
fn await_push(app: &mut App) {
    if report_pushes(app) < 1 {
        return;
    }
    println!(
        "[flutter_state] guest reported its first push after {:.2}s",
        app.start.elapsed().as_secs_f64()
    );
    app.settle_left = SETTLE_TURNS;
    app.step = Step::SettlePush;
}

/// How many pushes the guest has reported so far; 0 if it has not said.
fn report_pushes(app: &App) -> i64 {
    match field(app.last_report.as_ref(), "pushes") {
        Some(Value::Int(n)) => *n,
        _ => 0,
    }
}

/// Asserts the push count and payload once the settle window has elapsed.
fn settle_push(app: &mut App) {
    app.settle_left = app.settle_left.saturating_sub(1);
    if app.settle_left > 0 {
        return;
    }
    let pushes = report_pushes(app);
    app.results.push_arrived_once = Some((
        pushes == 1,
        format!("the guest reported {pushes} push(es) (want exactly 1)"),
    ));

    let path = top_level_str(app, "pushPath")
        .unwrap_or("<none>")
        .to_string();
    let value = field(app.last_report.as_ref(), "pushValue").cloned();
    app.results.push_carried_value = Some((
        path == COUNTER_PATH && value == Some(Value::Int(99)),
        format!("the push carried path={path:?} value={value:?} (want {COUNTER_PATH:?} / Int(99))"),
    ));
    app.step = Step::Report;
}

/// Reads a top-level `Value::Str` out of the guest's report.
fn top_level_str<'a>(app: &'a App, key: &str) -> Option<&'a str> {
    match field(app.last_report.as_ref(), key)? {
        Value::Str(s) => Some(s.as_str()),
        _ => None,
    }
}

/// Prints every claim as a PASS or FAIL line, returning how many failed.
fn print_report(app: &App) -> usize {
    println!();
    println!("=== Duet Flutter shared-state report ===");
    if let Some(step) = app.timed_out_at {
        println!(
            "TIMED OUT after {DEADLINE:?} while at stage {step:?}; last guest report = {:?}",
            app.last_report
        );
    }
    println!(
        "guest stage at exit: {:?}",
        stage(app).unwrap_or("<never published>")
    );
    println!(
        "notifications received for this subscriber: {}, sent to the guest: {}",
        app.notifications_seen, app.pushes_delivered
    );
    println!();

    let r = &app.results;
    let checks = [
        (
            "the Dart guest reached the host over duet/rpc",
            &r.guest_reached_host,
        ),
        (
            "a Dart-written value is readable from Rust",
            &r.rust_reads_dart_write,
        ),
        (
            "every probe f64 round-tripped bit-exactly",
            &r.floats_round_trip,
        ),
        (
            "hostile input over the real channel yields bounded failures",
            &r.hostile_bounded,
        ),
        (
            "a Rust write reached the guest exactly once",
            &r.push_arrived_once,
        ),
        (
            "that push carried the value Rust wrote",
            &r.push_carried_value,
        ),
    ];
    let failures = checks
        .iter()
        .filter(|(label, outcome)| !print_check(label, outcome))
        .count();
    println!();
    failures
}

/// Prints one claim, returning whether it passed. An unreached claim is a
/// failure, not a blank: a sequence that stopped early must not read as green.
fn print_check(label: &str, outcome: &Outcome) -> bool {
    match outcome {
        Some((true, detail)) => {
            println!("PASS: {label} - {detail}");
            true
        }
        Some((false, detail)) => {
            println!("FAIL: {label} - {detail}");
            false
        }
        None => {
            println!("FAIL: {label} - never reached; the sequence stopped before this stage");
            false
        }
    }
}
