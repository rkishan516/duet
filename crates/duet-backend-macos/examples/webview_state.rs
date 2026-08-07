//! The shared-state proof for the webview guest: a real `wry` `WebView`
//! running real JavaScript writes into the store, Rust reads the value back,
//! and then Rust's own write travels the other way as a push the guest
//! receives.
//!
//! Everything under `duet-webview` is tested without a webview: `duet-webview`'s
//! unit tests call [`duet_webview::handle_ipc_text`] directly, which proves the
//! *routing* but says nothing about whether `wry` ever hands a guest's
//! `window.ipc.postMessage` to that function, or whether an evaluated
//! `window.__duet.onResponse(...)` reaches the guest. This example is the first
//! thing in the workspace that closes both halves of that loop over the real
//! transport, in both directions:
//!
//! 1. guest -> host: JS calls `__duet.set("counter", {t:"i", v:"42"})`, the
//!    reply lands back in the guest, and **Rust then reads `Value::Int(42)`
//!    out of the store** — exact equality, not "changed".
//! 2. host -> guest: JS calls `__duet.subscribe("counter")`, **Rust** writes
//!    `Value::Int(99)`, and the guest's `window.__duet.pushes` reaches
//!    **exactly** length 1 carrying 99.
//!
//! Run: `cargo run -p duet-backend-macos --example webview_state`
//!
//! An **example**, not a test: `tao`'s event loop must be built on the
//! process's main thread, which `cargo test`'s harness does not provide (see
//! `src/sink.rs`'s `#[ignore]`d test for the same constraint in miniature).
//!
//! # What this proves, and what it cannot prove here
//!
//! Provable without a reachable on-screen WindowServer (see the crate root
//! docs): that JavaScript executes, that `with_ipc_handler` delivers guest
//! messages to Rust, that `evaluate_script` delivers host scripts to the
//! guest, and that the store is genuinely shared between the two. All of that
//! is in-process and observable from Rust.
//!
//! **Not** provable here, and not claimed: that a human sees any of it, and
//! that real mouse or keyboard input reaches the webview. Spike B found
//! synthetic input events reach a Flutter view but *not* a `WKWebView`, still
//! unexplained; nothing in this example touches input, so it neither confirms
//! nor contradicts that.
//!
//! # Two encoding hazards this file is deliberately shaped around
//!
//! **`Int` is a decimal string on the wire.** The JS below sends
//! `{t:"i", v:"42"}`, not `{t:"i", v:42}` — `duet-codec` encodes 64-bit
//! integers as strings so values above 2^53 survive JavaScript's number type.
//! A JSON number there is a decode failure, not a rounded value.
//!
//! **A probe script must return a plain object, never a JSON string.**
//! `wry`'s `evaluate_script_with_callback` runs the script's return value
//! through `NSJSONSerialization` on the native side, so returning
//! `JSON.stringify(x)` comes back double-encoded — quoted and escaped. Spike B
//! lost real time to exactly this (see `spikes/spike-b-macos/src/main.rs:688`).
//! [`PROBE_JS`] therefore returns an object literal. Note that the protocol
//! itself never travels this way: replies and pushes are *pushed in* via
//! [`WebviewSurface::eval`] with the payload embedded in the script, which has
//! no such re-serialization step.
//!
//! # Staged, not raced
//!
//! Like `examples/lifecycle.rs`, this drives a [`Step`] enum one stage per
//! run-loop turn. The sequence is asynchronous in both directions — an IPC
//! message crosses to Rust and its reply crosses back on a later turn — so
//! staging is what makes it assertable rather than flaky. A whole-run
//! [`DEADLINE`] backs that up: a hung example is indistinguishable from a slow
//! one and would wedge CI, so timing out reports *which stage* it stopped at
//! and exits non-zero rather than waiting forever.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant as StdInstant};

use duet_backend_macos::{DuetEvent, ProxySink, WebviewSurface};
use duet_core::{Notification, Path, SubscriberId, Value};
use duet_runtime::Runtime;
use tao::dpi::LogicalSize;
use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::window::{Window, WindowBuilder};

/// How long the whole sequence gets before it is declared hung. Generous: the
/// work here is a page load and four IPC round trips, all of which land in
/// well under a second when they land at all.
const DEADLINE: Duration = Duration::from_secs(30);

/// Milliseconds between run-loop turns. Each turn advances at most one stage
/// and issues one readback probe.
const TURN_MS: u64 = 50;

/// Turns to keep probing *after* the first push arrives, before asserting the
/// count. The claim is "exactly one push", and a duplicate arriving a turn
/// later would otherwise pass unnoticed.
const SETTLE_TURNS: u32 = 6;

/// Writes 42 from the guest. `{t:"i", v:"42"}` — a decimal **string**, per the
/// 2^53 guard described in the module docs.
const JS_SET: &str = r#"window.__duet.set("counter", {t:"i", v:"42"});"#;

/// Subscribes the guest to `counter`, so a later Rust write reaches it.
const JS_SUBSCRIBE: &str = r#"window.__duet.subscribe("counter");"#;

/// Reads the guest's observable state back out. Returns a plain object — see
/// the module docs on the double-encoding hazard. Tolerates being run before
/// the bootstrap script has executed, which is normal on the first turns.
const PROBE_JS: &str = r#"(function () {
  if (!window.__duet) { return { ready: false, logLen: 0, pushLen: 0 }; }
  var pushes = window.__duet.pushes;
  var last = pushes.length ? pushes[pushes.length - 1] : null;
  var patch = last && last.notification ? last.notification.patch : null;
  var value = patch ? patch.value : null;
  return {
    ready: true,
    logLen: window.__duet.log.length,
    pushLen: pushes.length,
    lastPath: patch ? patch.path : null,
    lastTag: value ? value.t : null,
    lastValue: value ? String(value.v) : null
  };
})()"#;

/// The guest's observable state, as of the last readback that completed.
#[derive(Debug, Default, Clone)]
struct Probe {
    /// Whether `window.__duet` exists yet, i.e. the bootstrap script ran.
    ready: bool,
    /// How many responses `onResponse` has received.
    log_len: u64,
    /// How many pushes `onPush` has received.
    push_len: u64,
    /// The path carried by the most recent push, if any.
    last_path: Option<String>,
    /// The tagged-value type of the most recent push (`"i"` for an int).
    last_tag: Option<String>,
    /// The most recent push's value, stringified in JS so a decimal-string
    /// int survives the trip unchanged.
    last_value: Option<String>,
    /// The raw JSON `wry` handed back, kept for the failure report.
    raw: String,
}

/// One assertion's outcome: `None` means the run never got that far.
type Outcome = Option<(bool, String)>;

/// Every claim this example makes, in report order.
#[derive(Debug, Default)]
struct Results {
    /// The bootstrap HTML loaded and its script defined `window.__duet`.
    guest_booted: Outcome,
    /// A guest IPC message reached Rust *and* its reply reached the guest.
    ipc_round_trip: Outcome,
    /// The headline: a JS-written value is readable from Rust, exactly.
    rust_reads_js_write: Outcome,
    /// A Rust write reached the guest as exactly one push.
    push_arrived_once: Outcome,
    /// That push carried 99.
    push_carried_value: Outcome,
}

/// The stages this example drives, one per run-loop turn. See the module docs
/// on why staging (rather than sleeping and hoping) is what makes an
/// asynchronous two-directional sequence assertable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    /// Poll until the guest's bootstrap script has defined `window.__duet`.
    AwaitBoot,
    /// Evaluate [`JS_SET`] in the guest.
    JsSet,
    /// Poll until the guest has received the reply to that `set`.
    AwaitSetReply,
    /// Read `counter` from Rust and assert it is exactly `Value::Int(42)`.
    AssertRustReadsJsWrite,
    /// Evaluate [`JS_SUBSCRIBE`] in the guest.
    JsSubscribe,
    /// Poll until the guest has received the reply to that `subscribe`.
    AwaitSubscribeReply,
    /// Write `Value::Int(99)` from Rust, which should notify the guest.
    RustWrite,
    /// Poll until the guest's first push arrives.
    AwaitPush,
    /// Keep probing a little longer, so a *second* push cannot slip past the
    /// "exactly one" claim.
    SettlePush,
    /// Print the report and exit.
    Report,
}

/// Everything the driver closure needs across turns of the event loop.
struct App {
    start: StdInstant,
    step: Step,
    /// Written by the readback callback (which `wry` requires to be
    /// `Send + 'static`, so it cannot borrow this struct) and read by the
    /// driver on the next turn.
    probe: Arc<Mutex<Probe>>,
    surface: WebviewSurface,
    /// Kept alive only so the webview it hosts stays alive.
    _window: Window,
    /// `Some` until the report shuts it down. `tao::EventLoop::run` never
    /// returns and drops nothing, so the core thread must be torn down
    /// explicitly.
    runtime: Option<Runtime>,
    subscriber: SubscriberId,
    counter: Path,
    results: Results,
    settle_left: u32,
    /// Notifications this surface's subscriber received from the store.
    notifications_seen: usize,
    /// Of those, how many were successfully evaluated into the guest.
    pushes_delivered: usize,
    /// The stage the deadline fired at, if it did.
    timed_out_at: Option<Step>,
}

fn main() {
    let event_loop = EventLoopBuilder::<DuetEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    let runtime = Runtime::spawn(
        Value::map([("counter", Value::Int(0))]),
        ProxySink::new(proxy.clone()),
    );
    let subscriber = runtime.next_subscriber_id();

    let window = match WindowBuilder::new()
        .with_title("Duet webview state example")
        .with_inner_size(LogicalSize::new(480.0, 320.0))
        .build(&event_loop)
    {
        Ok(w) => w,
        Err(e) => fail_setup(runtime, &format!("could not create a tao window: {e}")),
    };

    let surface = match WebviewSurface::new(&window, runtime.handle(), subscriber, proxy) {
        Ok(s) => s,
        Err(e) => fail_setup(runtime, &format!("could not create the wry webview: {e}")),
    };
    println!("[webview_state] window and wry webview created; subscriber={subscriber:?}");

    let counter = match Path::parse("counter") {
        Ok(p) => p,
        Err(e) => fail_setup(
            runtime,
            &format!("\"counter\" should be a valid path: {e:?}"),
        ),
    };

    let mut app = App {
        start: StdInstant::now(),
        step: Step::AwaitBoot,
        probe: Arc::new(Mutex::new(Probe::default())),
        surface,
        _window: window,
        runtime: Some(runtime),
        subscriber,
        counter,
        results: Results::default(),
        settle_left: 0,
        notifications_seen: 0,
        pushes_delivered: 0,
        timed_out_at: None,
    };

    event_loop.run(move |event, _target, control_flow| {
        drive(&mut app, event, control_flow);
    });
}

/// Reports a setup failure as a FAIL line and exits non-zero, rather than
/// panicking: a machine that cannot create a window or a webview is a finding
/// about the environment, and it should read the same way as any other
/// failure in this example's output.
fn fail_setup(runtime: Runtime, reason: &str) -> ! {
    println!("FAIL: setup - {reason}");
    if let Err(e) = runtime.shutdown() {
        println!("[webview_state] runtime shutdown failed: {e}");
    }
    std::process::exit(1);
}

/// The event-loop body. Two arms carry the protocol and neither is optional:
/// [`DuetEvent::WebviewScript`] **is** the reply path (the IPC handler cannot
/// hold the webview it answers through, since `wry` installs the handler
/// before `build()` yields the `WebView`), and [`DuetEvent::Notifications`] is
/// the push path.
fn drive(app: &mut App, event: Event<DuetEvent>, control_flow: &mut ControlFlow) {
    match event {
        Event::NewEvents(StartCause::Init) => {
            *control_flow = next_turn();
        }
        Event::NewEvents(StartCause::ResumeTimeReached { .. }) => {
            advance(app);
            *control_flow = next_turn();
        }
        Event::UserEvent(DuetEvent::WebviewScript { subscriber, script }) => {
            // Routed by subscriber even though this example owns exactly one
            // webview: `deliver` is where the check lives, and a driver that
            // called `eval` directly would be one more place two-guest routing
            // could go wrong the day a second surface appeared.
            if let Err(e) = app.surface.deliver(subscriber, &script) {
                println!("[webview_state] evaluating a host script failed: {e}");
            }
        }
        Event::UserEvent(DuetEvent::Notifications(batch)) => deliver_pushes(app, batch),
        Event::UserEvent(DuetEvent::Tick) => {}
        _ => {}
    }
}

/// Schedules the next run-loop turn.
fn next_turn() -> ControlFlow {
    ControlFlow::WaitUntil(std::time::Instant::now() + Duration::from_millis(TURN_MS))
}

/// Turns store notifications into guest pushes. Filters on this surface's own
/// subscriber: a notification for another guest must never be evaluated into
/// this one.
fn deliver_pushes(app: &mut App, batch: Vec<Notification>) {
    for note in batch {
        if note.subscriber != app.subscriber {
            continue;
        }
        app.notifications_seen += 1;
        match app.surface.push(&note) {
            Ok(()) => app.pushes_delivered += 1,
            Err(e) => println!("[webview_state] pushing a notification failed: {e}"),
        }
    }
}

/// Advances by at most one stage, then requests a fresh readback for the next
/// turn to act on.
fn advance(app: &mut App) {
    if app.step != Step::Report && app.start.elapsed() > DEADLINE {
        app.timed_out_at = Some(app.step);
        app.step = Step::Report;
    }

    let probe = read_probe(app);
    match app.step {
        Step::AwaitBoot => await_boot(app, &probe),
        Step::JsSet => app.step = eval_stage(app, "set", JS_SET, Step::AwaitSetReply),
        Step::AwaitSetReply => await_reply(app, &probe, 1, Step::AssertRustReadsJsWrite),
        Step::AssertRustReadsJsWrite => assert_rust_reads_js_write(app),
        Step::JsSubscribe => {
            app.step = eval_stage(app, "subscribe", JS_SUBSCRIBE, Step::AwaitSubscribeReply)
        }
        Step::AwaitSubscribeReply => await_reply(app, &probe, 2, Step::RustWrite),
        Step::RustWrite => rust_write(app),
        Step::AwaitPush => await_push(app, &probe),
        Step::SettlePush => settle_push(app, &probe),
        Step::Report => report_and_exit(app, &probe),
    }
    request_probe(app);
}

/// Copies the latest readback out from under its lock. A poisoned lock means
/// the callback panicked; treat that as "no data yet" rather than propagating,
/// so the deadline reports it instead of this turn panicking.
fn read_probe(app: &App) -> Probe {
    match app.probe.lock() {
        Ok(guard) => guard.clone(),
        Err(_) => Probe::default(),
    }
}

/// Asks the guest for a fresh readback; the callback lands on a later turn.
fn request_probe(app: &App) {
    let slot = Arc::clone(&app.probe);
    let outcome = app.surface.eval_with_callback(PROBE_JS, move |json| {
        if let Ok(mut guard) = slot.lock() {
            *guard = parse_probe(&json);
        }
    });
    if let Err(e) = outcome {
        println!("[webview_state] readback probe failed: {e}");
    }
}

/// Parses [`PROBE_JS`]'s return value. Total: anything unexpected becomes a
/// default `Probe` carrying the raw text, which the report prints.
fn parse_probe(json: &str) -> Probe {
    let parsed: serde_json::Value = serde_json::from_str(json).unwrap_or(serde_json::Value::Null);
    let string_at = |key: &str| parsed.get(key).and_then(|v| v.as_str()).map(str::to_string);
    Probe {
        ready: parsed
            .get("ready")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        log_len: parsed.get("logLen").and_then(|v| v.as_u64()).unwrap_or(0),
        push_len: parsed.get("pushLen").and_then(|v| v.as_u64()).unwrap_or(0),
        last_path: string_at("lastPath"),
        last_tag: string_at("lastTag"),
        last_value: string_at("lastValue"),
        raw: json.to_string(),
    }
}

/// Waits for the bootstrap script to define `window.__duet`.
fn await_boot(app: &mut App, probe: &Probe) {
    if !probe.ready {
        return;
    }
    app.results.guest_booted = Some((
        true,
        format!(
            "window.__duet exists after {:.2}s",
            app.start.elapsed().as_secs_f64()
        ),
    ));
    println!(
        "[webview_state] guest booted; probe readback = {}",
        probe.raw
    );
    app.step = Step::JsSet;
}

/// Evaluates one guest-side call, reporting an eval failure loudly. Returns
/// the stage to move to either way — a failed eval still has to reach the
/// report rather than stalling here.
fn eval_stage(app: &App, what: &str, script: &str, next: Step) -> Step {
    match app.surface.eval(script) {
        Ok(()) => println!("[webview_state] evaluated the guest's {what} call"),
        Err(e) => println!("[webview_state] evaluating the guest's {what} call failed: {e}"),
    }
    next
}

/// Waits until the guest has received `wanted` responses in total.
fn await_reply(app: &mut App, probe: &Probe, wanted: u64, next: Step) {
    if probe.log_len < wanted {
        return;
    }
    println!(
        "[webview_state] guest has received {} response(s) after {:.2}s",
        probe.log_len,
        app.start.elapsed().as_secs_f64()
    );
    // Reaching a *reply* proves the full loop: the guest's postMessage
    // reached the Rust ipc_handler, and the host's script reached the guest.
    // Recorded on the first reply only.
    if app.results.ipc_round_trip.is_none() {
        app.results.ipc_round_trip = Some((
            true,
            format!(
                "the guest received {} response(s) over wry IPC",
                probe.log_len
            ),
        ));
    }
    app.step = next;
}

/// The headline assertion: read `counter` from Rust and require exactly
/// `Value::Int(42)`.
fn assert_rust_reads_js_write(app: &mut App) {
    let read = match app.runtime.as_ref() {
        Some(rt) => rt.handle().get(&app.counter),
        None => {
            app.results.rust_reads_js_write =
                Some((false, "the runtime was already shut down".to_string()));
            app.step = Step::JsSubscribe;
            return;
        }
    };
    let outcome = match read {
        Ok(Some(Value::Int(42))) => (
            true,
            "Rust read counter = Int(42), written by JavaScript over wry IPC".to_string(),
        ),
        Ok(other) => (
            false,
            format!("Rust read counter = {other:?}, expected Some(Int(42))"),
        ),
        Err(e) => (false, format!("reading counter from Rust failed: {e}")),
    };
    println!("[webview_state] {}", outcome.1);
    app.results.rust_reads_js_write = Some(outcome);
    app.step = Step::JsSubscribe;
}

/// Writes 99 from Rust. The store notifies this surface's subscriber, which
/// `ProxySink` marshals onto this thread as [`DuetEvent::Notifications`].
fn rust_write(app: &mut App) {
    match app.runtime.as_ref() {
        Some(rt) => match rt.handle().set(&app.counter, Value::Int(99)) {
            Ok(()) => println!("[webview_state] Rust wrote counter = Int(99)"),
            Err(e) => println!("[webview_state] Rust write of counter failed: {e}"),
        },
        None => println!("[webview_state] Rust write skipped: runtime already shut down"),
    }
    app.step = Step::AwaitPush;
}

/// Waits for the guest's first push, then holds for [`SETTLE_TURNS`] more so a
/// duplicate cannot pass the "exactly one" claim.
fn await_push(app: &mut App, probe: &Probe) {
    if probe.push_len == 0 {
        return;
    }
    println!(
        "[webview_state] guest received its first push after {:.2}s: {}",
        app.start.elapsed().as_secs_f64(),
        probe.raw
    );
    app.settle_left = SETTLE_TURNS;
    app.step = Step::SettlePush;
}

/// Asserts the push count and payload once the settle window has elapsed.
fn settle_push(app: &mut App, probe: &Probe) {
    app.settle_left = app.settle_left.saturating_sub(1);
    if app.settle_left > 0 {
        return;
    }
    app.results.push_arrived_once = Some((
        probe.push_len == 1,
        format!(
            "window.__duet.pushes.length == {} (want exactly 1)",
            probe.push_len
        ),
    ));
    let path = probe.last_path.as_deref().unwrap_or("<none>");
    let tag = probe.last_tag.as_deref().unwrap_or("<none>");
    let value = probe.last_value.as_deref().unwrap_or("<none>");
    app.results.push_carried_value = Some((
        path == "counter" && tag == "i" && value == "99",
        format!(
            "the push carried path={path:?} value={{t:{tag:?}, v:{value:?}}} (want counter / i / 99)"
        ),
    ));
    app.step = Step::Report;
}

/// Prints every claim as a PASS or FAIL line and exits, non-zero if anything
/// failed. Exiting explicitly rather than returning: `tao::EventLoop::run`
/// never returns, so this is the only place a process exit code can be set.
fn report_and_exit(app: &mut App, probe: &Probe) -> ! {
    if let Some(rt) = app.runtime.take() {
        if let Err(e) = rt.shutdown() {
            println!("[webview_state] runtime shutdown failed: {e}");
        }
    }

    println!();
    println!("=== Duet webview shared-state report ===");
    if let Some(step) = app.timed_out_at {
        println!(
            "TIMED OUT after {:?} while at stage {step:?}; last readback = {}",
            DEADLINE, probe.raw
        );
    }
    println!(
        "notifications received for this subscriber: {}, evaluated into the guest: {}",
        app.notifications_seen, app.pushes_delivered
    );
    println!();

    let r = &app.results;
    let checks = [
        ("guest bootstrap ran", &r.guest_booted),
        (
            "wry IPC round trip (guest -> host -> guest)",
            &r.ipc_round_trip,
        ),
        (
            "a JS-written value is readable from Rust",
            &r.rust_reads_js_write,
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
    if failures == 0 {
        println!("ALL PASS: a JavaScript guest and Rust share one store over real wry IPC");
        std::process::exit(0);
    }
    println!("{failures} check(s) FAILED");
    std::process::exit(1);
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
