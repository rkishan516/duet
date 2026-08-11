//! **The command-RPC proof over a real transport**: a live `wry` `WebView`
//! running real JavaScript invokes real `#[command]` functions on a real host,
//! and one of them writes the store Rust then reads back.
//!
//! Everything about command RPC before this ran over **stdio**:
//! `crates/duet-host-stdio` wraps `duet_protocol::handle_text_with` in a
//! process speaking newline-delimited JSON, and the Dart and JavaScript
//! conformance suites drive it through a pipe. That proves the protocol and the
//! guest clients. It says nothing about whether an `invoke` survives the one
//! transport a shipped Duet application actually uses — `wry`'s IPC handler in,
//! an evaluated script out — because until this increment a
//! [`WebviewSurface`] called `handle_text`, which refuses every command by
//! construction.
//!
//! Run:
//!
//! ```text
//! FLUTTER_LINUX_RENDERER=software GDK_BACKEND=x11 cargo run -p duet-backend-linux --example webview_commands
//! ```
//!
//! The header is uniform across this crate's examples: under WSLg
//! `FLUTTER_LINUX_RENDERER=software` is required wherever a Flutter engine
//! boots (no usable GL there — see the crate docs), and GL-capable desktops
//! may omit it. This example boots no Flutter engine, so here the variable is
//! inert.
//!
//! An **example**, not a test: this needs a real window and a pumped GTK main
//! loop. Linux `tao` can build a loop on a worker thread (`with_any_thread`;
//! `src/sink.rs`'s tests use it for the closed-loop contract), but
//! `cargo test`'s harness provides neither a window nor a pumped loop, and a
//! WebKitGTK webview needs both.
//!
//! # What is claimed here
//!
//! 1. **Arguments arrive under the right names.** `subtract(a, b)` rather than
//!    an `add`, deliberately: subtraction is not commutative, so a guest whose
//!    argument encoder swapped the two keys answers `-7` where `7` is expected.
//!    An `add` would have agreed with a completely broken encoder.
//! 2. **A typed error survives as a value.** `raise` returns `Err`, and what
//!    reaches the guest is a `raised` carrying a structured map it can match
//!    on — not a `failed` carrying prose, which is not reversible.
//! 3. **A command body and the guest share one store.** `bump` reads `counter`
//!    through the [`StoreHandle`](duet_runtime::StoreHandle) its context hands
//!    it, writes it back, and **Rust then reads the new value** — exact
//!    equality, not "changed".
//! 4. **A command this surface was not given does not exist for it.**
//!    Authorization in Duet is by construction: the guest names a command, and
//!    what resolves is decided entirely by the table the embedder passed to
//!    [`WebviewSurface::with_commands`]. An unregistered name is answered
//!    `failed`, never `raised`.
//!
//! # What this cannot prove here, and does not claim
//!
//! Nothing about real input. Unlike the macOS reference machine — which had no
//! reachable on-screen WindowServer for spawned processes — this machine has a
//! real display session: a `tao` window is created, `wry` renders into it, and
//! WSLg genuinely presents it on the desktop. What an autonomous run cannot
//! supply is a human at the keyboard: nothing here touches mouse or keyboard
//! input.
//!
//! It also drives the **bootstrap** guest page (`duet-webview`'s
//! `BOOTSTRAP_HTML`), not the generated TypeScript client. The generated client
//! is driven against a live host in `packages/duet-js/test/live-host.test.ts`,
//! over stdio; what this file adds is the transport, not the client.
//!
//! # Staged, not raced
//!
//! Like its siblings, this drives a [`Step`] enum one stage per run-loop turn:
//! an IPC message crosses to Rust and its reply crosses back on a later turn,
//! so staging is what makes an asynchronous sequence assertable rather than
//! flaky. A whole-run [`DEADLINE`] backs it up — a hung example is
//! indistinguishable from a slow one and would wedge CI, so timing out reports
//! *which stage* it stopped at and exits non-zero.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant as StdInstant};

use duet::{CommandContext, CommandEntry, Path, SharedState, Value, command, commands};
use duet_backend_linux::{DuetEvent, ProxySink, WebviewSurface};
use duet_runtime::Runtime;
use tao::dpi::LogicalSize;
use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::window::{Window, WindowBuilder};

/// How long the whole sequence gets before it is declared hung.
const DEADLINE: Duration = Duration::from_secs(30);

/// Milliseconds between run-loop turns.
const TURN_MS: u64 = 50;

/// The structured error [`raise`] returns, as the guest must decode it.
///
/// A map with a string and an integer in it, not a bare string: the whole claim
/// `raised` makes over `failed` is that a typed error survives as something a
/// guest can match on, and a single string would be indistinguishable from the
/// prose a `failed` already carries.
#[derive(SharedState, Debug)]
struct Unlucky {
    code: String,
    short_by: i64,
}

/// `subtract(a, b) -> a - b`, saturating.
#[command]
fn subtract(a: i64, b: i64) -> i64 {
    a.saturating_sub(b)
}

/// A command that runs and returns `Err`.
#[command]
fn raise() -> Result<(), Unlucky> {
    Err(Unlucky {
        code: "unlucky".to_string(),
        short_by: 42,
    })
}

/// `bump(path, by)`: reads the integer at `path`, adds `by`, writes it back.
///
/// The command that matters most here. It reaches the store through the handle
/// its [`CommandContext`] carries — which works only because a handler runs on
/// the thread that called `dispatch_with`, and for a webview surface that is
/// `wry`'s IPC handler — a WebKitGTK script-message callback on the GTK main
/// thread — never the runtime's core thread.
#[command]
fn bump(ctx: &CommandContext, path: String, by: i64) -> Result<i64, Value> {
    let parsed = Path::parse(&path).map_err(|e| refusal("bad_path", &e.to_string()))?;
    let current: i64 = match ctx.store().get(&parsed) {
        Ok(Some(Value::Int(n))) => n,
        Ok(_) => return Err(refusal("not_an_integer", &path)),
        Err(e) => return Err(refusal("store", &e.to_string())),
    };
    let next = current.saturating_add(by);
    if let Err(e) = ctx.store().set(&parsed, Value::Int(next)) {
        return Err(refusal("store", &e.to_string()));
    }
    Ok(next)
}

/// A structured error value, since this command's declared error type is
/// `dynamic`.
fn refusal(code: &str, detail: &str) -> Value {
    Value::map([
        ("code", Value::Str(code.to_string())),
        ("detail", Value::Str(detail.to_string())),
    ])
}

/// The registry this surface is built with — and therefore the whole of what
/// the guest can reach.
///
/// A `static` array in read-only data: [`CommandEntry::of`] is a `const fn`, so
/// there is no initialization order to reason about and no way for the table to
/// be assembled twice or not at all.
static COMMANDS: [CommandEntry; 3] = commands![subtract, raise, bump];

/// The name the guest asks for that is deliberately **not** in [`COMMANDS`].
///
/// A near-miss of a registered name, so a refusal cannot be mistaken for the
/// request being malformed.
const UNREGISTERED: &str = "subtrac";

/// `subtract(10, 3)`. The arguments are written in the order `b`, `a` on
/// purpose: what reaches the host is the bootstrap's canonical sort, not this
/// literal's order, and a host that bound by position would answer `-7`.
const JS_SUBTRACT: &str = r#"window.__duet.invoke("subtract",
  { b: {t:"i", v:"3"}, a: {t:"i", v:"10"} });"#;

/// `raise()`, which takes nothing and always fails.
const JS_RAISE: &str = r#"window.__duet.invoke("raise");"#;

/// `bump("counter", 5)` against a store seeded with `counter = 0`.
const JS_BUMP: &str = r#"window.__duet.invoke("bump",
  { path: {t:"s", v:"counter"}, by: {t:"i", v:"5"} });"#;

/// A command this surface was never given.
const JS_UNKNOWN: &str = r#"window.__duet.invoke("subtrac");"#;

/// Reads the guest's response log back out.
///
/// Returns a plain object — never `JSON.stringify` — because `wry` runs a
/// script's return value through WebKitGTK's JavaScript-result JSON
/// serialization, so a returned JSON string comes back double-encoded. macOS
/// Spike B lost real time to exactly that, and the rule holds verbatim on
/// WebKitGTK (see [`WebviewSurface::eval_with_callback`]).
///
/// Tolerates running before the bootstrap script has executed, which is normal
/// on the first turns.
const PROBE_JS: &str = r#"(function () {
  if (!window.__duet) { return { ready: false, logLen: 0, entries: [] }; }
  var log = window.__duet.log;
  var entries = log.map(function (r) {
    var payload = r.value !== undefined ? r.value : r.error;
    var scalar = payload && typeof payload.v !== "object" ? String(payload.v) : "";
    var code = payload && payload.t === "m" && payload.v && payload.v.code
      ? String(payload.v.code.v) : "";
    var shortBy = payload && payload.t === "m" && payload.v && payload.v.short_by
      ? String(payload.v.short_by.v) : "";
    return {
      kind: String(r.kind),
      tag: payload && payload.t ? String(payload.t) : "",
      value: scalar,
      code: code,
      shortBy: shortBy
    };
  });
  return { ready: true, logLen: log.length, entries: entries };
})()"#;

/// One reply the guest received, as the probe reports it.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Reply {
    kind: String,
    tag: String,
    value: String,
    code: String,
    short_by: String,
}

/// The guest's observable state, as of the last readback that completed.
#[derive(Debug, Default, Clone)]
struct Probe {
    ready: bool,
    replies: Vec<Reply>,
    /// The raw JSON `wry` handed back, kept for the failure report.
    raw: String,
}

/// One assertion's outcome: `None` means the run never got that far.
type Outcome = Option<(bool, String)>;

/// Every claim this example makes, in report order.
#[derive(Debug, Default)]
struct Results {
    guest_booted: Outcome,
    subtract_returned: Outcome,
    raise_carried_a_typed_error: Outcome,
    bump_returned: Outcome,
    rust_reads_the_commands_write: Outcome,
    unregistered_was_refused: Outcome,
}

/// The stages this example drives, one per run-loop turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    /// Poll until the bootstrap script has defined `window.__duet`.
    AwaitBoot,
    /// Invoke `subtract`, then wait for its reply and assert on it.
    InvokeSubtract,
    AwaitSubtract,
    /// Invoke `raise`, then wait and assert.
    InvokeRaise,
    AwaitRaise,
    /// Invoke `bump`, then wait, assert the reply, and read the store.
    InvokeBump,
    AwaitBump,
    /// Invoke a name this surface was never given.
    InvokeUnknown,
    AwaitUnknown,
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
    counter: Path,
    results: Results,
    /// The stage the deadline fired at, if it did.
    timed_out_at: Option<Step>,
}

fn main() {
    let event_loop = EventLoopBuilder::<DuetEvent>::with_user_event().build();
    // The metronome. tao's GTK loop parks a pending `WaitUntil` inside a
    // fully blocking `gtk_main_iteration` with no timer source armed for the
    // deadline, so on a quiet session — here, a webview whose page has
    // settled — a 50 ms turn stalls until some unrelated X11 event happens
    // to wake the context: measured at ~10 s per turn under WSLg before this
    // thread existed. A proxy send is a glib channel source and *does* wake
    // the context, so a thread posting `DuetEvent::Tick` at turn cadence
    // keeps the deadline checks honest; the Tick events themselves are
    // no-ops.
    let metronome = event_loop.create_proxy();
    std::thread::spawn(move || {
        while metronome.send_event(DuetEvent::Tick).is_ok() {
            std::thread::sleep(Duration::from_millis(TURN_MS));
        }
    });
    let proxy = event_loop.create_proxy();
    let runtime = Runtime::spawn(
        Value::map([("counter", Value::Int(0))]),
        ProxySink::new(proxy.clone()),
    );
    let subscriber = runtime.next_subscriber_id();

    let window = match WindowBuilder::new()
        .with_title("Duet webview commands example")
        .with_inner_size(LogicalSize::new(480.0, 320.0))
        .build(&event_loop)
    {
        Ok(w) => w,
        Err(e) => fail_setup(runtime, &format!("could not create a tao window: {e}")),
    };

    // `with_commands` builds the webview into the tao window's default vbox —
    // spike finding L-F7, handled inside the surface: built against the window
    // itself, a WebKitGTK webview runs invisibly.
    let surface = match WebviewSurface::with_commands(
        &window,
        runtime.handle(),
        subscriber,
        proxy,
        &COMMANDS,
    ) {
        Ok(s) => s,
        Err(e) => fail_setup(runtime, &format!("could not create the wry webview: {e}")),
    };
    println!(
        "[webview_commands] webview created with {} commands",
        COMMANDS.len()
    );

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
        counter,
        results: Results::default(),
        timed_out_at: None,
    };

    event_loop.run(move |event, _target, control_flow| {
        drive(&mut app, event, control_flow);
    });
}

/// Reports a setup failure as a FAIL line and exits non-zero.
fn fail_setup(runtime: Runtime, reason: &str) -> ! {
    println!("FAIL: setup - {reason}");
    if let Err(e) = runtime.shutdown() {
        println!("[webview_commands] runtime shutdown failed: {e}");
    }
    std::process::exit(1);
}

/// The event-loop body. [`DuetEvent::WebviewScript`] **is** the reply path: the
/// IPC handler cannot hold the webview it answers through, since `wry` installs
/// the handler before `build_gtk` yields the `WebView`.
fn drive(app: &mut App, event: Event<DuetEvent>, control_flow: &mut ControlFlow) {
    match event {
        Event::NewEvents(StartCause::Init) => *control_flow = next_turn(),
        Event::NewEvents(StartCause::ResumeTimeReached { .. }) => {
            advance(app);
            *control_flow = next_turn();
        }
        Event::UserEvent(DuetEvent::WebviewScript { subscriber, script }) => {
            // Routed by subscriber even with one webview here: the check lives
            // on the surface, so an embedder with two of them cannot deliver
            // one guest's reply to the other by forgetting it.
            if let Err(e) = app.surface.deliver(subscriber, &script) {
                println!("[webview_commands] evaluating a host script failed: {e}");
            }
        }
        // Nothing subscribes in this example: the claims are about `invoke`,
        // and the push path already has two examples of its own.
        Event::UserEvent(DuetEvent::Notifications(_) | DuetEvent::Tick) => {}
        _ => {}
    }
}

/// Schedules the next run-loop turn.
fn next_turn() -> ControlFlow {
    ControlFlow::WaitUntil(std::time::Instant::now() + Duration::from_millis(TURN_MS))
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
        Step::InvokeSubtract => {
            app.step = eval_stage(app, "subtract", JS_SUBTRACT, Step::AwaitSubtract)
        }
        Step::AwaitSubtract => await_reply(app, &probe, 1, assert_subtract, Step::InvokeRaise),
        Step::InvokeRaise => app.step = eval_stage(app, "raise", JS_RAISE, Step::AwaitRaise),
        Step::AwaitRaise => await_reply(app, &probe, 2, assert_raise, Step::InvokeBump),
        Step::InvokeBump => app.step = eval_stage(app, "bump", JS_BUMP, Step::AwaitBump),
        Step::AwaitBump => await_reply(app, &probe, 3, assert_bump, Step::InvokeUnknown),
        Step::InvokeUnknown => {
            app.step = eval_stage(
                app,
                "an unregistered command",
                JS_UNKNOWN,
                Step::AwaitUnknown,
            )
        }
        Step::AwaitUnknown => await_reply(app, &probe, 4, assert_unknown, Step::Report),
        Step::Report => report_and_exit(app, &probe),
    }
    request_probe(app);
}

/// Copies the latest readback out from under its lock. A poisoned lock means
/// the callback panicked; treat that as "no data yet" so the deadline reports
/// it rather than this turn panicking.
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
        println!("[webview_commands] readback probe failed: {e}");
    }
}

/// Parses [`PROBE_JS`]'s return value. Total: anything unexpected becomes a
/// default [`Probe`] carrying the raw text, which the report prints.
fn parse_probe(json: &str) -> Probe {
    let parsed: serde_json::Value = serde_json::from_str(json).unwrap_or(serde_json::Value::Null);
    let text = |entry: &serde_json::Value, key: &str| {
        entry
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string()
    };
    let replies = parsed
        .get("entries")
        .and_then(|v| v.as_array())
        .map(|entries| {
            entries
                .iter()
                .map(|entry| Reply {
                    kind: text(entry, "kind"),
                    tag: text(entry, "tag"),
                    value: text(entry, "value"),
                    code: text(entry, "code"),
                    short_by: text(entry, "shortBy"),
                })
                .collect()
        })
        .unwrap_or_default();
    Probe {
        ready: parsed
            .get("ready")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        replies,
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
    app.step = Step::InvokeSubtract;
}

/// Evaluates one guest-side call, reporting an eval failure loudly. Returns the
/// stage to move to either way — a failed eval still has to reach the report
/// rather than stalling here.
fn eval_stage(app: &App, what: &str, script: &str, next: Step) -> Step {
    match app.surface.eval(script) {
        Ok(()) => println!("[webview_commands] guest invoked {what}"),
        Err(e) => println!("[webview_commands] invoking {what} failed: {e}"),
    }
    next
}

/// Waits until the guest has received `wanted` replies, then runs `assert` over
/// the last one and moves to `next`.
fn await_reply(
    app: &mut App,
    probe: &Probe,
    wanted: usize,
    assert: fn(&mut App, &Reply),
    next: Step,
) {
    if probe.replies.len() < wanted {
        return;
    }
    let Some(reply) = probe.replies.get(wanted - 1) else {
        return;
    };
    println!(
        "[webview_commands] reply {wanted} after {:.2}s: {reply:?}",
        app.start.elapsed().as_secs_f64()
    );
    assert(app, reply);
    app.step = next;
}

/// `subtract(10, 3)` must be `returned` with exactly 7.
fn assert_subtract(app: &mut App, reply: &Reply) {
    let ok = reply.kind == "returned" && reply.tag == "i" && reply.value == "7";
    app.results.subtract_returned = Some((
        ok,
        format!(
            "subtract(a=10, b=3) answered kind={:?} {{t:{:?}, v:{:?}}} (want returned / i / 7; \
             a host binding by position would answer -7)",
            reply.kind, reply.tag, reply.value
        ),
    ));
}

/// `raise` must be `raised`, carrying the struct rather than prose.
fn assert_raise(app: &mut App, reply: &Reply) {
    let ok = reply.kind == "raised"
        && reply.tag == "m"
        && reply.code == "unlucky"
        && reply.short_by == "42";
    app.results.raise_carried_a_typed_error = Some((
        ok,
        format!(
            "raise() answered kind={:?} code={:?} short_by={:?} (want raised / unlucky / 42, \
             as a map the guest can match on)",
            reply.kind, reply.code, reply.short_by
        ),
    ));
}

/// `bump("counter", 5)` must be `returned` 5, and the store must hold 5.
fn assert_bump(app: &mut App, reply: &Reply) {
    let ok = reply.kind == "returned" && reply.tag == "i" && reply.value == "5";
    app.results.bump_returned = Some((
        ok,
        format!(
            "bump(path=\"counter\", by=5) answered kind={:?} {{t:{:?}, v:{:?}}} (want returned / i / 5)",
            reply.kind, reply.tag, reply.value
        ),
    ));

    // The headline: the command body wrote the same store this process owns.
    let read = match app.runtime.as_ref() {
        Some(rt) => rt.handle().get(&app.counter),
        None => {
            app.results.rust_reads_the_commands_write =
                Some((false, "the runtime was already shut down".to_string()));
            return;
        }
    };
    app.results.rust_reads_the_commands_write = Some(match read {
        Ok(Some(Value::Int(5))) => (
            true,
            "Rust read counter = Int(5), written by a command body a JavaScript guest invoked"
                .to_string(),
        ),
        Ok(other) => (
            false,
            format!("Rust read counter = {other:?}, expected Some(Int(5))"),
        ),
        Err(e) => (false, format!("reading counter from Rust failed: {e}")),
    });
}

/// A name this surface was never given must be `failed`, never `raised`.
fn assert_unknown(app: &mut App, reply: &Reply) {
    app.results.unregistered_was_refused = Some((
        reply.kind == "failed",
        format!(
            "invoking {UNREGISTERED:?} answered kind={:?} (want failed: the host would not run it, \
             which is a different event from one that ran and failed)",
            reply.kind
        ),
    ));
}

/// Prints every claim as a PASS or FAIL line and exits, non-zero if anything
/// failed. Exiting explicitly rather than returning: `tao::EventLoop::run` never
/// returns, so this is the only place a process exit code can be set.
fn report_and_exit(app: &mut App, probe: &Probe) -> ! {
    if let Some(rt) = app.runtime.take() {
        if let Err(e) = rt.shutdown() {
            println!("[webview_commands] runtime shutdown failed: {e}");
        }
    }

    println!();
    println!("=== Duet webview command-RPC report ===");
    if let Some(step) = app.timed_out_at {
        println!(
            "TIMED OUT after {DEADLINE:?} while at stage {step:?}; last readback = {}",
            probe.raw
        );
    }
    println!();

    let r = &app.results;
    let checks = [
        ("guest bootstrap ran", &r.guest_booted),
        (
            "a command's arguments arrive under the right names",
            &r.subtract_returned,
        ),
        (
            "a command that ran and failed reaches the guest as a typed raise",
            &r.raise_carried_a_typed_error,
        ),
        (
            "a command body's return reaches the guest",
            &r.bump_returned,
        ),
        (
            "that body wrote the store this process reads",
            &r.rust_reads_the_commands_write,
        ),
        (
            "a command this surface was not given does not exist for it",
            &r.unregistered_was_refused,
        ),
    ];
    let failures = checks
        .iter()
        .filter(|(label, outcome)| !print_check(label, outcome))
        .count();

    println!();
    if failures == 0 {
        println!("ALL PASS: a JavaScript guest invoked real host commands over real wry IPC");
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
