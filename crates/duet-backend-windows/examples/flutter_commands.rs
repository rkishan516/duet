//! **Command RPC over a real Flutter platform channel**: a real `FlutterEngine`
//! (`flutter_windows.dll`) running real Dart invokes real `#[command]`
//! functions on a real host, and one of them writes the store both sides read.
//!
//! The sibling of `examples/webview_commands.rs`, which makes the same claims
//! for the *other* guest. Both exist because the two transports are separate
//! code: a webview guest speaks `wry`'s IPC and is answered by evaluating
//! JavaScript, and a Dart guest speaks a `FlutterDesktopMessenger` channel and
//! is answered through the message's single-use response handle. A proof for
//! one says nothing about the other.
//!
//! # Build and run
//!
//! The Dart guest is a tracked fixture and must be compiled to a Windows
//! `data` directory first — a debug/JIT build (`flutter_assets/` carrying
//! `kernel_blob.bin`, plus `icudtl.dat`), the only kind this project has ever
//! produced:
//!
//! ```text
//! (cd fixtures/duet_guest && flutter build windows --debug)
//! DUET_FLUTTER_BUNDLE=fixtures/duet_guest/build/windows/x64/runner/Debug/data \
//!   cargo run -p duet-backend-windows --example flutter_commands
//! ```
//!
//! That bundle carries every Dart driver; this example selects the command
//! one by seeding [`MODE_PATH`] with [`COMMANDS_MODE`] (see
//! `fixtures/duet_guest/lib/guest_support.dart`).
//!
//! # What is claimed here
//!
//! 1. **Arguments arrive under the right names.** `subtract(a, b)` rather than
//!    an `add`, deliberately: subtraction is not commutative, so a guest whose
//!    argument encoder swapped the two keys answers `-7` where `7` is expected.
//! 2. **A typed error survives as a value.** `raise` returns `Err`, and the Dart
//!    guest receives a `DuetRaised` carrying a structured map it can match on.
//! 3. **A command body and the guest share one store.** `bump` reads
//!    [`COUNTER_PATH`] through the handle its context carries and writes it
//!    back; the guest reads the new value through an ordinary `get`, and **Rust
//!    reads it too** — exact equality, not "changed".
//! 4. **A command this surface was not given does not exist for it.** An
//!    unregistered name is a `failed`, which the Dart client raises as a
//!    `DuetFailure` — never a `DuetRaised`, which would say something ran.
//!
//! # How the guest reports back
//!
//! No side channel and no log scraping: the Dart driver publishes one map at
//! [`REPORT_PATH`] over the very channel under test, and this file polls that
//! path out of the shared store. A report that arrives at all is already
//! evidence the guest -> host direction works.
//!
//! # What this cannot prove here, and does not claim
//!
//! Nothing visual. The engine runs **headless** — created and `Run` with no
//! view controller ever attached; Windows needs no `allowHeadlessExecution`
//! analog, the C API simply composes that way
//! (`spikes/spike-b-windows/FINDINGS.md` W-F3) — because none of these claims
//! need one. Only a debug/JIT bundle (`kernel_blob.bin`) has ever been built
//! in this environment, so nothing here says anything about release/AOT Dart.

use std::time::{Duration, Instant};

use duet::{CommandContext, CommandEntry, Path, SharedState, Value, command, commands};
use duet_backend_windows::{FlutterSurface, WinBackend};
use duet_host::WindowBackend;
use duet_runtime::{NullSink, Runtime, StoreHandle};
use duet_supervisor::SurfaceId;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, MSG, MsgWaitForMultipleObjects, PM_REMOVE, PeekMessageW, QS_ALLINPUT,
    TranslateMessage,
};

/// How long the whole sequence gets before it is declared hung.
const DEADLINE: Duration = Duration::from_secs(60);

/// Seconds of message pump per turn. This is what gives the Dart isolate its
/// thread: without it the guest never runs and nothing here can happen.
const TURN_SECS: f64 = 0.05;

/// The store path the guest publishes its whole report at.
///
/// Must match `kCommandReportPath` in
/// `fixtures/duet_guest/lib/command_driver.dart`.
const REPORT_PATH: &str = "dartCommands";

/// The path the guest asks `bump` to increment.
///
/// Must match `kBumpPath` in the Dart driver.
const COUNTER_PATH: &str = "counter";

/// How much the guest asks `bump` to add. Must match `kBumpBy` in the Dart
/// driver.
const BUMP_BY: i64 = 7;

/// Where the host writes the value that selects a Dart driver.
const MODE_PATH: &str = "mode";

/// The [`MODE_PATH`] value that selects the command driver.
///
/// Must match `kCommandsMode` in `fixtures/duet_guest/lib/guest_support.dart`.
const COMMANDS_MODE: &str = "commands";

/// The structured error [`raise`] returns, as the Dart guest must decode it.
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
/// It reaches the store through the handle its [`CommandContext`] carries —
/// which works only because a handler runs on the thread that called
/// `dispatch_with`, and for a Flutter surface that is the engine's own
/// platform-thread callback, never the runtime's core thread.
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
/// the Dart guest can reach. `subtrac`, which the guest also asks for, is
/// deliberately absent.
static COMMANDS: [CommandEntry; 3] = commands![subtract, raise, bump];

/// One assertion's outcome: `None` means the run never got that far.
type Outcome = Option<(bool, String)>;

/// Every claim this example makes, in report order.
#[derive(Debug, Default)]
struct Results {
    guest_reported: Outcome,
    subtract_returned: Outcome,
    raise_carried_a_typed_error: Outcome,
    bump_returned: Outcome,
    guest_read_the_commands_write: Outcome,
    rust_reads_the_commands_write: Outcome,
    unregistered_was_refused: Outcome,
}

/// Path to the Flutter `data` directory produced by `flutter build windows`
/// (`flutter_assets/` + `icudtl.dat`, next to the runner exe).
///
/// Canonicalized to an absolute path before use: the engine resolves relative
/// paths against the running EXE (`target/debug/examples/`), not the current
/// directory, so a relative path would pass the backend's existence checks and
/// then boot an engine with no assets. The spike hit exactly this; its
/// canonicalize-or-keep fallback is carried over
/// (`spikes/spike-b-windows/src/main.rs`, `flutter_data_dir`).
fn flutter_bundle_path() -> String {
    let raw = std::env::var("DUET_FLUTTER_BUNDLE")
        .unwrap_or_else(|_| "fixtures/duet_guest/build/windows/x64/runner/Debug/data".to_string());
    match std::path::Path::new(&raw).canonicalize() {
        Ok(abs) => abs.to_string_lossy().into_owned(),
        Err(_) => raw,
    }
}

/// The store the host seeds.
///
/// `mode` selects the Dart driver; `counter` is what `bump` increments; the
/// report key is seeded so the guest's one whole-map write lands in an existing
/// slot — `set` never creates intermediate nodes.
fn initial_root() -> Value {
    Value::map([
        (COUNTER_PATH, Value::Int(0)),
        (MODE_PATH, Value::Str(COMMANDS_MODE.to_string())),
        (REPORT_PATH, Value::Null),
    ])
}

fn main() {
    let bundle = flutter_bundle_path();
    println!("[flutter_commands] Flutter data directory: {bundle}");

    // `NullSink`: nothing here subscribes, because every claim is about
    // `invoke`. The push path already has three examples of its own.
    let runtime = Runtime::spawn(initial_root(), NullSink);
    let subscriber = runtime.next_subscriber_id();

    let report = match Path::parse(REPORT_PATH) {
        Ok(p) => p,
        Err(e) => fail_setup(runtime, &format!("{REPORT_PATH:?} must be a path: {e:?}")),
    };
    let counter = match Path::parse(COUNTER_PATH) {
        Ok(p) => p,
        Err(e) => fail_setup(runtime, &format!("{COUNTER_PATH:?} must be a path: {e:?}")),
    };

    let surface_id = SurfaceId::from_raw(1);
    let mut backend = WinBackend::new(&bundle);
    if let Err(e) = backend.start_renderer(surface_id) {
        fail_setup(runtime, &format!("could not boot a Flutter engine: {e}"));
    }
    let Some(engine) = backend.engine(surface_id) else {
        fail_setup(
            runtime,
            "the backend reported a booted renderer but has no engine for it",
        );
    };
    let surface =
        match FlutterSurface::with_commands(engine, runtime.handle(), subscriber, &COMMANDS) {
            Ok(s) => s,
            Err(e) => fail_setup(
                runtime,
                &format!("could not register the duet/rpc handler: {e}"),
            ),
        };
    println!(
        "[flutter_commands] engine booted headless; duet/rpc registered with {} commands",
        COMMANDS.len()
    );

    let store = runtime.handle();
    let start = Instant::now();
    let mut published: Option<Value> = None;
    while start.elapsed() < DEADLINE {
        pump_run_loop(TURN_SECS);
        if let Some(value) = finished_report(&store, &report) {
            published = Some(value);
            break;
        }
    }
    println!(
        "[flutter_commands] guest report after {:.2}s: {published:?}",
        start.elapsed().as_secs_f64()
    );

    let results = assess(published.as_ref(), &store, &counter);
    let failures = print_report(&results, start.elapsed() >= DEADLINE);

    // Teardown order is not interchangeable: the surface must unregister its
    // channel handler (its `Drop`) before the engine shuts down, because the
    // engine holds the handler's `user_data` pointer past registration — the C
    // analog of the macOS handler map that `shutDownEngine` never cleared.
    drop(surface);
    if let Err(e) = backend.destroy_renderer(surface_id) {
        println!("[flutter_commands] destroying the renderer failed: {e}");
    }
    if let Err(e) = runtime.shutdown() {
        println!("[flutter_commands] runtime shutdown failed: {e}");
    }

    if failures == 0 {
        println!(
            "ALL PASS: a Dart guest invoked real host commands over a real Flutter platform channel"
        );
        std::process::exit(0);
    }
    println!("{failures} check(s) FAILED");
    std::process::exit(1);
}

/// Reports a setup failure as a FAIL line and exits non-zero.
fn fail_setup(runtime: Runtime, reason: &str) -> ! {
    println!("FAIL: setup - {reason}");
    if let Err(e) = runtime.shutdown() {
        println!("[flutter_commands] runtime shutdown failed: {e}");
    }
    std::process::exit(1);
}

/// Pumps this thread's Win32 message queue for `secs`, which is what gives the
/// Dart isolate its thread back: the engine's platform task runner posts to a
/// hidden window on this thread, and its tasks are processed "transparently
/// through DispatchMessage" (the header's own note — see
/// `spikes/spike-b-windows/FINDINGS.md`).
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

/// The guest's report once it says every command has been invoked.
///
/// Waiting on the `stage` field rather than on the key merely existing is what
/// sequences the two sides: a report read mid-write would be a snapshot of a
/// driver that had not finished.
fn finished_report(store: &StoreHandle, report: &Path) -> Option<Value> {
    let held = store.get(report).ok()??;
    let Value::Map(entries) = &held else {
        return None;
    };
    match entries.get("stage") {
        Some(Value::Str(stage)) if stage == "invoked" || stage == "done" => Some(held.clone()),
        _ => None,
    }
}

/// The string at `key` in the guest's report, if it holds one.
fn text<'a>(report: Option<&'a Value>, key: &str) -> Option<&'a str> {
    match report? {
        Value::Map(entries) => match entries.get(key)? {
            Value::Str(s) => Some(s.as_str()),
            _ => None,
        },
        _ => None,
    }
}

/// The value at `key` in the guest's report.
fn field<'a>(report: Option<&'a Value>, key: &str) -> Option<&'a Value> {
    match report? {
        Value::Map(entries) => entries.get(key),
        _ => None,
    }
}

/// The string at `outer.inner` in the guest's report.
fn nested<'a>(report: Option<&'a Value>, outer: &str, inner: &str) -> Option<&'a str> {
    match field(report, outer)? {
        Value::Map(entries) => match entries.get(inner)? {
            Value::Str(s) => Some(s.as_str()),
            _ => None,
        },
        _ => None,
    }
}

/// The integer at `outer.inner` in the guest's report.
fn nested_int(report: Option<&Value>, outer: &str, inner: &str) -> Option<i64> {
    match field(report, outer)? {
        Value::Map(entries) => match entries.get(inner)? {
            Value::Int(n) => Some(*n),
            _ => None,
        },
        _ => None,
    }
}

/// Turns the guest's report, plus one read of the store, into every claim.
fn assess(report: Option<&Value>, store: &StoreHandle, counter: &Path) -> Results {
    let guest_reported = Some((
        report.is_some(),
        match text(report, "stage") {
            Some(stage) => format!("the guest published a report at stage {stage:?}"),
            None => "the guest never published a finished report".to_string(),
        },
    ));

    let subtract_value = field(report, "subtractValue");
    let subtract_returned = Some((
        text(report, "subtractKind") == Some("returned") && subtract_value == Some(&Value::Int(7)),
        format!(
            "subtract(a=10, b=3) answered {:?} carrying {subtract_value:?} \
             (want \"returned\" / Int(7); a host binding by position would answer -7)",
            text(report, "subtractKind")
        ),
    ));

    let raise_carried_a_typed_error = Some((
        text(report, "raiseKind") == Some("raised")
            && nested(report, "raiseError", "code") == Some("unlucky")
            && nested_int(report, "raiseError", "short_by") == Some(42),
        format!(
            "raise() answered {:?} with code={:?} short_by={:?} (want \"raised\" / \
             \"unlucky\" / 42, as a map the guest can match on)",
            text(report, "raiseKind"),
            nested(report, "raiseError", "code"),
            nested_int(report, "raiseError", "short_by"),
        ),
    ));

    let bumped = Value::Int(BUMP_BY);
    let bump_returned = Some((
        text(report, "bumpKind") == Some("returned") && field(report, "bumpValue") == Some(&bumped),
        format!(
            "bump(path={COUNTER_PATH:?}, by={BUMP_BY}) answered {:?} carrying {:?} \
             (want \"returned\" / {bumped:?})",
            text(report, "bumpKind"),
            field(report, "bumpValue"),
        ),
    ));

    let guest_read_the_commands_write = Some((
        field(report, "bumpReadBack") == Some(&bumped),
        format!(
            "the guest read {COUNTER_PATH:?} back as {:?} after the command wrote it (want {bumped:?})",
            field(report, "bumpReadBack"),
        ),
    ));

    let rust_reads_the_commands_write = Some(match store.get(counter) {
        Ok(Some(Value::Int(n))) if n == BUMP_BY => (
            true,
            format!(
                "Rust read {COUNTER_PATH} = Int({BUMP_BY}), written by a command body a Dart \
                 guest invoked"
            ),
        ),
        Ok(other) => (
            false,
            format!("Rust read {COUNTER_PATH} = {other:?}, expected Some(Int({BUMP_BY}))"),
        ),
        Err(e) => (
            false,
            format!("reading {COUNTER_PATH} from Rust failed: {e}"),
        ),
    });

    let unregistered_was_refused = Some((
        text(report, "unknownKind") == Some("failed"),
        format!(
            "invoking an unregistered command answered {:?} (want \"failed\": the host would \
             not run it, which is a different event from one that ran and failed)",
            text(report, "unknownKind")
        ),
    ));

    Results {
        guest_reported,
        subtract_returned,
        raise_carried_a_typed_error,
        bump_returned,
        guest_read_the_commands_write,
        rust_reads_the_commands_write,
        unregistered_was_refused,
    }
}

/// Prints every claim as a PASS or FAIL line, returning how many failed.
fn print_report(results: &Results, timed_out: bool) -> usize {
    println!();
    println!("=== Duet Flutter command-RPC report ===");
    if timed_out {
        println!("TIMED OUT after {DEADLINE:?} waiting for the guest's report");
    }
    println!();

    let checks = [
        (
            "the Dart guest published its report",
            &results.guest_reported,
        ),
        (
            "a command's arguments arrive under the right names",
            &results.subtract_returned,
        ),
        (
            "a command that ran and failed reaches the guest as a typed raise",
            &results.raise_carried_a_typed_error,
        ),
        (
            "a command body's return reaches the guest",
            &results.bump_returned,
        ),
        (
            "the guest reads that body's write back through the store",
            &results.guest_read_the_commands_write,
        ),
        (
            "Rust reads that body's write too, so it is one store",
            &results.rust_reads_the_commands_write,
        ),
        (
            "a command this surface was not given does not exist for it",
            &results.unregistered_was_refused,
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
