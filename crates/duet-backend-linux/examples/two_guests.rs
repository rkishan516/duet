//! The isolation proof: **two live guests at once**, in one process, against
//! one store — a real `wry` webview running JavaScript and a real
//! `FlutterEngine` running Dart — and the confidentiality boundary between
//! them, exercised rather than assumed.
//!
//! Both transports were already proven, separately. `examples/webview_state.rs`
//! shares the store with a JavaScript guest over `wry` IPC;
//! `examples/flutter_state.rs` shares it with a Dart guest over a Flutter
//! platform channel. Nothing in this project had ever run both at the same
//! time, so every claim about one guest not disturbing another rested on
//! reading the code. This file is the first thing that runs the two together
//! and checks.
//!
//! # Build and run
//!
//! The Dart guest is a tracked fixture and must be compiled to a Linux
//! Flutter bundle first — a debug/JIT build, the only kind this project has
//! ever produced (release/AOT behaviour here is unverified):
//!
//! ```text
//! (cd fixtures/duet_guest && flutter build linux --debug)
//! DUET_FLUTTER_BUNDLE=fixtures/duet_guest/build/linux/x64/debug/bundle/data \
//!   FLUTTER_LINUX_RENDERER=software GDK_BACKEND=x11 \
//!   cargo run -p duet-backend-linux --example two_guests
//! ```
//!
//! `DUET_FLUTTER_BUNDLE` defaults to that same path, so the env var is only
//! needed when running from another directory. `FLUTTER_LINUX_RENDERER=software`
//! is required under WSLg — there is no usable GL there ("Could not determine
//! GL version") — while GL-capable desktops may omit it; `GDK_BACKEND=x11`
//! keeps GTK on the X11 path this port was verified under. That bundle's
//! `flutter_assets` carries **both** Dart drivers; this example selects the
//! two-guest one by seeding [`MODE_PATH`] with [`DUET_MODE`] (see
//! `fixtures/duet_guest/lib/guest_support.dart`).
//!
//! # What is claimed here
//!
//! 1. **Both guests can write, and each other's writes are visible.** The
//!    webview writes [`FROM_JS_PATH`], the Dart guest writes
//!    [`FROM_DART_PATH`], and then **each guest reads the other's key** and
//!    reports the exact value it saw. Not "the store changed" — the specific
//!    integer the other renderer wrote.
//! 2. **Each guest receives only its own notifications.** Both guests
//!    subscribe to [`BOTH_PATH`]; one Rust write must produce **exactly one**
//!    push for each, and each push must carry **that guest's own**
//!    `subscription` id. Then each guest subscribes to a path the other does
//!    not ([`JS_ONLY_PATH`], [`DART_ONLY_PATH`]) and a write there must leave
//!    the other guest's push count untouched. The second half is the real
//!    test: the first half would pass unchanged against a host with no
//!    filtering at all.
//! 3. **Guest B cannot unsubscribe guest A.** The webview fires raw
//!    `unsubscribe` requests across every id from [`ATTACK_ID_MIN`] to
//!    [`ATTACK_ID_MAX`] and the Dart guest must still receive its next push.
//! 4. **Tearing one guest down does not disturb the other.** The webview
//!    surface is dropped outright; the Dart guest must still receive a push
//!    afterwards, and [`Runtime::shutdown`] must then succeed cleanly.
//!
//! # Assertion 3 guards a vulnerability that was real and reproduced
//!
//! This is not a hypothetical. Earlier on this branch a guest **could**
//! destroy another guest's subscription by naming its id — `SubscriptionId`s
//! are small sequential integers allocated from **0**
//! (`duet_core::Store::subscribe`), so an attacker needs no information at
//! all, only a loop. The reproduction printed
//! `>>> guest A subscriptions remaining: 0`. The fix scoped removal to the
//! **host-supplied** subscriber (`duet_protocol::dispatch`'s `Unsubscribe`
//! arm), and `duet-protocol` pins it at unit level in
//! `one_guest_cannot_unsubscribe_another_guests_subscription`. What was still
//! missing is what this file adds: the same attack driven from a *real second
//! renderer*, as raw text a hostile guest could actually put on the wire, with
//! a *real first renderer* on the other side to survive it.
//!
//! The attack is asserted on **delivery, not on a count**. A test that only
//! checked "guest A still has N subscriptions" could pass because nothing was
//! ever written; this one requires the exact value Rust wrote after the attack
//! to arrive at the Dart guest.
//!
//! The swept range deliberately covers the **attacker's own** subscriptions
//! too, and the sequence then checks that the webview stopped receiving its
//! own pushes. That is the positive control without which assertion 3 would be
//! vacuous: a host that simply never removed any subscription would pass the
//! attack test while being completely broken.
//!
//! # The confidentiality filter is exercised, not sidestepped
//!
//! [`deliver_pushes`] hands **every** notification to **both** surfaces and
//! lets each one filter. That is the point. [`WebviewSurface::push`] and
//! [`FlutterSurface::push`] both take a [`Notification`] rather than an
//! already-addressed payload precisely so a caller cannot leak one guest's
//! state by forgetting a check, and a driver that filtered at the call site
//! would be testing its own `if` rather than the boundary. Both siblings
//! (`webview_state.rs`, `flutter_state.rs`) filter at the call site because
//! they have only one guest and nothing to confuse; here there are two, and
//! the fan-out is deliberate.
//!
//! # What this cannot prove here, and does not claim
//!
//! Nothing about real input. Unlike the macOS reference environment (which had
//! no reachable on-screen WindowServer), WSLg provides a real display session:
//! the `tao` window genuinely appears on screen and `wry` renders into it.
//! What stays unverified is real mouse and keyboard input — these runs are
//! autonomous, with nobody at the keyboard — and synthetic input has not been
//! measured on this platform at all, so input is out of scope here entirely.
//! The Flutter engine runs **headless** — booted with no window open for its
//! surface, so its implicit view realizes into a private hidden holding window
//! (boot IS the view's realization on Linux, L-F1/L-F2) — because none of
//! these claims need a view. That choice is one-way: a realized `FlView` can
//! never move into a real window afterwards (L-F3), and nothing here ever asks
//! it to; teardown goes through `destroy_renderer`, which shuts the engine
//! down the same way either way.
//!
//! Only a debug/JIT Linux bundle (`flutter_assets/` with `kernel_blob.bin`)
//! has ever been built in this environment, so nothing here says anything
//! about release/AOT Dart.
//!
//! # One event loop, two renderers
//!
//! `wry` needs a `tao` event loop; `flutter_state.rs` has none and iterates
//! the GTK main context directly. They cannot both be true in one process, so
//! this file uses the `tao` loop as the single driver and boots the Flutter
//! engine alongside it — the arrangement Spike B measured
//! (`spikes/spike-b-linux`: a `tao` event loop, a Flutter engine and a `wry`
//! webview coexisting on one main thread, 722/722 proxy events over 180 s).
//! That also means one [`ProxySink`] serves both guests, which is exactly the
//! shape the fan-out above needs.
//!
//! # Staged, not raced
//!
//! Like both siblings, this drives a [`Step`] enum one stage per run-loop
//! turn — with two independent, asynchronous guests to sequence rather than
//! one, which is what makes staging the difference between an assertion and a
//! coin flip. A whole-run [`DEADLINE`] backs it up: a hung example is
//! indistinguishable from a slow one and would wedge CI, so timing out reports
//! *which stage* it stopped at, prints both guests' last observed state, and
//! exits non-zero rather than waiting forever.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant as StdInstant};

use duet_backend_linux::{DuetEvent, FlutterSurface, LinuxBackend, ProxySink, WebviewSurface};
use duet_core::{Notification, Path, SubscriberId, Value};
use duet_host::WindowBackend;
use duet_runtime::Runtime;
use duet_supervisor::SurfaceId;
use tao::dpi::LogicalSize;
use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::window::{Window, WindowBuilder};

/// How long the whole sequence gets before it is declared hung.
///
/// Deliberately **shorter** than the Dart guest's own observation window
/// (`kDuetObserveWindow`, 60 s): a timeout must land while that guest is still
/// publishing, so the report prints its live state rather than a snapshot from
/// after it gave up. The work here — an engine boot, a page load, a handshake
/// and roughly two dozen staged turns — lands in well under 20 seconds when it
/// lands at all. (It only does so with the metronome thread in `main` — see
/// the comment there for the tao-under-WSLg stall this example first ran
/// into.)
const DEADLINE: Duration = Duration::from_secs(45);

/// Milliseconds between run-loop turns. Each turn advances at most one stage.
const TURN_MS: u64 = 50;

/// Turns to keep polling *after* an expected push arrives, before asserting
/// counts. Every count claim here is an exact number, and a duplicate or a
/// leaked push arriving a moment later would otherwise pass unnoticed. Sixteen
/// turns is 800 ms — far longer than either guest needs to notice a push and
/// republish.
const SETTLE_TURNS: u32 = 16;

/// Where the host tells the Dart fixture which driver to run. Must match
/// `kModePath` in `fixtures/duet_guest/lib/guest_support.dart`.
const MODE_PATH: &str = "mode";

/// The [`MODE_PATH`] value that selects the fixture's two-guest driver. Must
/// match `kDuetMode` there.
const DUET_MODE: &str = "duet";

/// Where the Dart guest publishes everything it observes. Must match
/// `kDuetReportPath` in `fixtures/duet_guest/lib/duet_driver.dart`.
const DART_REPORT_PATH: &str = "dartGuest";

/// The parent of the two cross-visibility keys. Seeded as a map because
/// `Value::set` never creates intermediate nodes — it only inserts an absent
/// *final* map key — so neither guest could create `shared` itself.
const SHARED_PATH: &str = "shared";

/// Where the **webview** guest writes, for the Dart guest to read.
const FROM_JS_PATH: &str = "shared.fromJs";

/// Where the **Dart** guest writes, for the webview guest to read. Must match
/// `kFromDartPath` in the fixture.
const FROM_DART_PATH: &str = "shared.fromDart";

/// What the webview guest writes at [`FROM_JS_PATH`].
const JS_WRITES: i64 = 111;

/// What the Dart guest writes at [`FROM_DART_PATH`]. Must match `kDartWrites`
/// in the fixture. Distinct from [`JS_WRITES`] so that "each guest read the
/// other's key" cannot be satisfied by a guest reading back its own write.
const DART_WRITES: i64 = 222;

/// The path **both** guests subscribe to.
const BOTH_PATH: &str = "both";

/// The path only the **webview** guest subscribes to.
const JS_ONLY_PATH: &str = "jsOnly";

/// The path only the **Dart** guest subscribes to. Must match
/// `kDartOnlyPath` in the fixture.
const DART_ONLY_PATH: &str = "dartOnly";

/// The values Rust writes, one per staged write. All distinct, and none equal
/// to a seeded value: `duet-core` emits **minimal** patches, so re-writing a
/// value a path already holds produces no notification at all, and an
/// assertion waiting on that push would hang for reasons that have nothing to
/// do with isolation.
const WROTE_BOTH: i64 = 7;
/// Written at [`DART_ONLY_PATH`] while only the Dart guest watches it.
const WROTE_DART_ONLY: i64 = 8;
/// Written at [`JS_ONLY_PATH`] while only the webview guest watches it.
const WROTE_JS_ONLY: i64 = 9;
/// Written at [`DART_ONLY_PATH`] after the webview's hostile unsubscribe.
const WROTE_AFTER_ATTACK: i64 = 10;
/// Written at [`JS_ONLY_PATH`] after the attack, to check the attacker
/// unsubscribed *itself*.
const WROTE_JS_AFTER_ATTACK: i64 = 12;
/// Written at [`DART_ONLY_PATH`] after the webview surface is dropped.
const WROTE_AFTER_TEARDOWN: i64 = 11;

/// The lowest subscription id the webview tries to cancel.
///
/// **Zero, not one.** `Store::subscribe` allocates ids from 0
/// (`crates/duet-core/src/store.rs`), so the very first subscription this
/// process hands out is id 0 and an attacker starting at 1 would miss it.
const ATTACK_ID_MIN: u64 = 0;

/// The highest subscription id the webview tries to cancel, inclusive.
///
/// This run creates four subscriptions in total, so `0..=10` covers every live
/// one with room to spare — which is precisely the attacker's advantage that
/// made the original vulnerability trivial to exploit: no information is
/// needed, only a loop.
const ATTACK_ID_MAX: u64 = 10;

/// How many hostile requests the sweep sends.
const ATTACK_COUNT: u64 = ATTACK_ID_MAX - ATTACK_ID_MIN + 1;

// The four scripts the driver evaluates in the webview guest. All built by
// interpolating the constants above rather than repeating their values, so a
// path or a number can be changed in exactly one place. That costs a little
// brace-doubling in the JS below and buys the guarantee that what the guest
// does and what this file asserts can never describe different things.

/// Writes [`JS_WRITES`] at [`FROM_JS_PATH`] from the webview guest.
///
/// The value travels as `{t:"i", v:"111"}` — a decimal **string**, because
/// `duet-codec` carries 64-bit integers as strings so values above 2^53 survive
/// JavaScript's number type. A JSON number there is a decode failure, not a
/// rounded value.
fn set_js() -> String {
    format!(r#"window.__duet.set("{FROM_JS_PATH}", {{t:"i", v:"{JS_WRITES}"}});"#)
}

/// Reads the **Dart** guest's key from the webview guest and stashes the
/// answer where [`PROBE_JS`] can see it, as `"<tag>:<value>"`.
///
/// Stringified in JS rather than compared there: the whole claim is that the
/// exact value the *other renderer* wrote is visible, and Rust must be the one
/// to decide whether it matches.
fn cross_read_js() -> String {
    format!(
        r#"window.__duet.get("{FROM_DART_PATH}").then(function (r) {{
  var v = r && r.value ? r.value : null;
  window.__duet.crossRead = v ? (String(v.t) + ":" + String(v.v)) : "absent";
}});"#
    )
}

/// Subscribes the webview guest to the shared path and to its own.
fn subscribe_js() -> String {
    format!(
        r#"window.__duet.subs = window.__duet.subs || {{}};
window.__duet.subscribe("{BOTH_PATH}").then(function (r) {{
  window.__duet.subs.both = r.subscription;
}});
window.__duet.subscribe("{JS_ONLY_PATH}").then(function (r) {{
  window.__duet.subs.jsOnly = r.subscription;
}});"#
    )
}

/// The attack: raw `unsubscribe` requests for every id from [`ATTACK_ID_MIN`]
/// to [`ATTACK_ID_MAX`], and request ids far above anything the bootstrap's own
/// counter reaches so an attack's reply can never be mistaken for a legitimate
/// call's.
///
/// Posted straight through `window.ipc.postMessage`, bypassing the bootstrap's
/// own `__duet` client entirely — which has no `unsubscribe` at all. That is
/// deliberate and load-bearing: a guest is a separate renderer running content
/// the host does not control, and nothing constrains what it puts on the wire.
/// Proving the host refuses this through its *own* client would prove much
/// less. (The Dart fixture's hostile-input probe bypasses `DuetClient` for the
/// same reason.)
///
/// The swept bounds come from the constants above, so [`ATTACK_COUNT`] cannot
/// drift out of step with what the guest actually sends.
fn attack_js() -> String {
    format!(
        r#"window.__duet.attacks = 0;
for (var i = {ATTACK_ID_MIN}; i <= {ATTACK_ID_MAX}; i++) {{
  window.ipc.postMessage(JSON.stringify({{
    kind: "unsubscribe",
    id: String(2000 + i),
    subscription: String(i)
  }}));
  window.__duet.attacks++;
}}"#
    )
}

/// Reads the webview guest's observable state back out. Returns a plain
/// object, never a JSON string: `wry`'s `evaluate_script_with_callback` runs a
/// script's return value through the platform's JSON serialization, so a
/// returned `JSON.stringify(x)` comes back double-encoded (macOS Spike B lost
/// real time to exactly this, and the rule holds verbatim on WebKitGTK — see
/// [`WebviewSurface::eval_with_callback`]). Tolerates running before the
/// bootstrap script has executed, which is normal on the first turns.
const PROBE_JS: &str = r#"(function () {
  if (!window.__duet) { return { ready: false }; }
  var d = window.__duet;
  var pushes = d.pushes;
  var last = pushes.length ? pushes[pushes.length - 1] : null;
  var note = last && last.notification ? last.notification : null;
  var patch = note ? note.patch : null;
  var value = patch ? patch.value : null;
  var subs = d.subs || {};
  return {
    ready: true,
    logLen: d.log.length,
    pushLen: pushes.length,
    lastPath: patch ? patch.path : null,
    lastTag: value ? value.t : null,
    lastValue: value ? String(value.v) : null,
    lastSubscriber: note ? String(note.subscriber) : null,
    lastSubscription: note ? String(note.subscription) : null,
    crossRead: d.crossRead === undefined ? null : d.crossRead,
    subBoth: subs.both === undefined ? null : String(subs.both),
    subJsOnly: subs.jsOnly === undefined ? null : String(subs.jsOnly),
    attacks: d.attacks === undefined ? 0 : d.attacks
  };
})()"#;

/// The webview guest's observable state, as of the last readback that
/// completed.
#[derive(Debug, Default, Clone)]
struct Probe {
    /// Whether `window.__duet` exists yet, i.e. the bootstrap script ran.
    ready: bool,
    /// How many responses `onResponse` has received, over the guest's whole
    /// life.
    log_len: u64,
    /// How many pushes `onPush` has received.
    push_len: u64,
    /// The path carried by the most recent push.
    last_path: Option<String>,
    /// The tagged-value type of the most recent push (`"i"` for an int).
    last_tag: Option<String>,
    /// The most recent push's value, stringified in JS so a decimal-string int
    /// survives the trip unchanged.
    last_value: Option<String>,
    /// The most recent push's `subscriber` field.
    last_subscriber: Option<String>,
    /// The most recent push's `subscription` field — the field assertion 2
    /// turns on.
    last_subscription: Option<String>,
    /// What this guest read back from the *Dart* guest's key, as
    /// `"<tag>:<value>"`.
    cross_read: Option<String>,
    /// This guest's subscription id for [`BOTH_PATH`].
    sub_both: Option<String>,
    /// This guest's subscription id for [`JS_ONLY_PATH`].
    sub_js_only: Option<String>,
    /// How many hostile `unsubscribe` messages this guest has posted.
    attacks: u64,
    /// The raw JSON `wry` handed back, kept for the failure report.
    raw: String,
}

/// One assertion's outcome: `None` means the run never got that far.
type Outcome = Option<(bool, String)>;

/// Every claim this example makes, in report order.
#[derive(Debug, Default)]
struct Results {
    /// Both renderers booted and reached the host.
    both_guests_booted: Outcome,
    /// The Dart guest read the webview's write, exactly.
    dart_sees_the_webviews_write: Outcome,
    /// The webview guest read the Dart guest's write, exactly.
    webview_sees_the_dart_write: Outcome,
    /// One write to a shared path produced exactly one push for each guest.
    shared_write_pushed_once_to_each: Outcome,
    /// Each of those pushes carried its own guest's subscription id.
    each_push_carried_its_own_subscription: Outcome,
    /// A write only the Dart guest watches never reached the webview.
    dart_only_write_stayed_away_from_the_webview: Outcome,
    /// A write only the webview watches never reached the Dart guest.
    js_only_write_stayed_away_from_flutter: Outcome,
    /// The host answered every hostile `unsubscribe` — proof the attack
    /// actually arrived rather than being lost in transit.
    hostile_unsubscribes_reached_the_host: Outcome,
    /// The headline regression guard: the Dart guest still received its push.
    flutter_survived_the_webviews_attack: Outcome,
    /// The positive control: the attacker's *own* subscriptions really were
    /// removed, so the claim above is not "nothing is ever removed".
    the_attacker_unsubscribed_itself: Outcome,
    /// The Dart guest still received a push after the webview was dropped.
    flutter_survived_the_webviews_teardown: Outcome,
    /// The core thread shut down cleanly at the end.
    runtime_shutdown_was_clean: Outcome,
}

/// The stages this example drives, one per run-loop turn. See the module docs
/// on why staging (rather than sleeping and hoping) is what makes two
/// independent asynchronous guests assertable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    /// Poll until the webview's bootstrap script has defined `window.__duet`.
    AwaitWebBoot,
    /// Evaluate [`set_js`] in the webview guest.
    WebWrite,
    /// Poll until the webview has received the reply to that `set`.
    AwaitWebSetReply,
    /// Poll until the Dart guest reports it has written its own key.
    AwaitDartWrote,
    /// Evaluate [`cross_read_js`] in the webview guest.
    WebCrossRead,
    /// Poll until both guests have published what they read, then assert
    /// claim 1 in both directions.
    AwaitCrossReads,
    /// Evaluate [`subscribe_js`] in the webview guest.
    WebSubscribe,
    /// Poll until both guests report they are subscribed.
    AwaitArmed,
    /// Write [`WROTE_BOTH`] at the path both guests watch.
    WriteBoth,
    /// Poll until both guests report a push.
    AwaitBothPushes,
    /// Settle, then assert claim 2's first half: exactly one push each,
    /// carrying each guest's own subscription id.
    SettleBoth,
    /// Write [`WROTE_DART_ONLY`] where only the Dart guest is watching.
    WriteDartOnly,
    /// Poll until the Dart guest reports its second push.
    AwaitDartOnlyPush,
    /// Settle, then assert the webview's count did **not** move.
    SettleDartOnly,
    /// Write [`WROTE_JS_ONLY`] where only the webview is watching.
    WriteJsOnly,
    /// Poll until the webview reports its second push.
    AwaitJsOnlyPush,
    /// Settle, then assert the Dart guest's count did **not** move.
    SettleJsOnly,
    /// Evaluate [`attack_js`]: hostile `unsubscribe` across every id from
    /// [`ATTACK_ID_MIN`] to [`ATTACK_ID_MAX`].
    WebAttack,
    /// Poll until the host has answered every one of them.
    AwaitAttackAnswered,
    /// Write to both single-guest paths: one to prove the victim survived, one
    /// to prove the attacker really did cancel its own subscription.
    WriteAfterAttack,
    /// Poll until the Dart guest reports the exact value written after the
    /// attack. **Delivery, not a count.**
    AwaitDartPushAfterAttack,
    /// Settle, then assert claim 3 and its positive control.
    SettleAfterAttack,
    /// Drop the webview surface and its window.
    TearDownWebview,
    /// Write [`WROTE_AFTER_TEARDOWN`] at the Dart guest's path.
    WriteAfterTeardown,
    /// Poll until that value reaches the Dart guest, then assert claim 4.
    AwaitDartPushAfterTeardown,
    /// Tear the Flutter side down, shut the runtime down, print, exit.
    Report,
}

/// The store paths this example uses, parsed once.
struct Paths {
    from_js: Path,
    from_dart: Path,
    both: Path,
    js_only: Path,
    dart_only: Path,
    dart_report: Path,
}

/// Everything the driver closure needs across turns of the event loop.
struct App {
    start: StdInstant,
    step: Step,
    /// Written by the readback callback (which `wry` requires to be
    /// `Send + 'static`, so it cannot borrow this struct) and read by the
    /// driver on the next turn.
    probe: Arc<Mutex<Probe>>,
    /// `None` once [`Step::TearDownWebview`] has dropped it — which is itself
    /// what claim 4 is about.
    webview: Option<WebviewSurface>,
    /// Dropped alongside the surface. Kept only so the webview it hosts stays
    /// alive.
    window: Option<Window>,
    /// `None` once [`Step::Report`] has dropped it. Must be dropped **before**
    /// the engine shuts down — the messenger holds the handler's `user_data`
    /// pointer past registration, and `destroy_renderer` forces the engine's
    /// dispose, after which there is no live channel table to unregister
    /// from (see [`FlutterSurface`]'s `Drop`).
    flutter: Option<FlutterSurface>,
    backend: LinuxBackend,
    flutter_surface: SurfaceId,
    /// `Some` until the report shuts it down. `tao::EventLoop::run` never
    /// returns and drops nothing, so the core thread must be torn down
    /// explicitly.
    runtime: Option<Runtime>,
    web_sub: SubscriberId,
    dart_sub: SubscriberId,
    paths: Paths,
    /// The Dart guest's last published report.
    dart_report: Option<Value>,
    results: Results,
    settle_left: u32,
    /// `log_len` observed just before the attack, so "the host answered all of
    /// them" is measured as a delta rather than an absolute.
    log_len_before_attack: u64,
    /// How many notifications the store addressed to each guest. Bookkeeping
    /// only — routing never consults it (see [`deliver_pushes`]).
    notes_for_web: usize,
    notes_for_dart: usize,
    /// Notifications addressed to neither guest. Must stay 0; anything else
    /// would mean the store is notifying a subscriber this example never
    /// created.
    notes_for_nobody: usize,
    /// Errors either surface reported while being handed a notification.
    push_errors: usize,
    /// The stage the deadline fired at, if it did.
    timed_out_at: Option<Step>,
}

/// Path to the Flutter `data` directory produced by `flutter build linux`
/// (containing `flutter_assets/` and `icudtl.dat`). Overridable via env var
/// for other machines, matching the sibling examples.
fn flutter_data_dir() -> String {
    std::env::var("DUET_FLUTTER_BUNDLE")
        .unwrap_or_else(|_| "fixtures/duet_guest/build/linux/x64/debug/bundle/data".to_string())
}

/// The store both guests share.
///
/// `shared` is a map because `Value::set` never creates intermediate nodes —
/// it only inserts an absent *final* map key — so neither guest could bring
/// `shared` into existence on its own. The three watched paths are seeded with
/// values distinct from everything Rust later writes, so no staged write is a
/// no-op (`duet-core` emits minimal patches, and a write of an unchanged value
/// notifies nobody).
fn initial_root() -> Value {
    Value::map([
        (MODE_PATH, Value::Str(DUET_MODE.to_string())),
        (
            SHARED_PATH,
            Value::map([("fromJs", Value::Null), ("fromDart", Value::Null)]),
        ),
        (BOTH_PATH, Value::Int(0)),
        (JS_ONLY_PATH, Value::Int(0)),
        (DART_ONLY_PATH, Value::Int(0)),
        (DART_REPORT_PATH, Value::Null),
    ])
}

fn main() {
    let data_dir = flutter_data_dir();
    println!("[two_guests] Flutter data dir: {data_dir}");

    let event_loop = EventLoopBuilder::<DuetEvent>::with_user_event().build();
    // The metronome. tao's GTK loop parks a pending `WaitUntil` inside a
    // fully blocking `gtk_main_iteration` with no timer source armed for the
    // deadline, so on a quiet session — a headless Flutter guest and a
    // settled webview page produce almost no wakeups — a 50 ms turn stalls
    // until some unrelated X11 event happens to wake the context: measured
    // at ~10 s per turn under WSLg before this thread existed, which parked
    // this very example mid-sequence past its whole deadline. A proxy send
    // is a glib channel source and *does* wake the context, so a thread
    // posting `DuetEvent::Tick` at turn cadence keeps the deadline checks
    // honest; the Tick events themselves are no-ops.
    let metronome = event_loop.create_proxy();
    std::thread::spawn(move || {
        while metronome.send_event(DuetEvent::Tick).is_ok() {
            std::thread::sleep(Duration::from_millis(TURN_MS));
        }
    });
    let proxy = event_loop.create_proxy();
    let runtime = Runtime::spawn(initial_root(), ProxySink::new(proxy.clone()));

    // Two subscribers from one runtime: this is the identity each surface is
    // built with and the only thing that distinguishes the two guests to the
    // store. Neither guest can name one — `duet_protocol::Request` has no
    // subscriber field — so these are the host's alone.
    let web_sub = runtime.next_subscriber_id();
    let dart_sub = runtime.next_subscriber_id();

    let paths = match parse_paths() {
        Ok(p) => p,
        Err(e) => fail_setup(runtime, &e),
    };

    let window = match WindowBuilder::new()
        .with_title("Duet two-guest isolation example")
        .with_inner_size(LogicalSize::new(480.0, 320.0))
        .build(&event_loop)
    {
        Ok(w) => w,
        Err(e) => fail_setup(runtime, &format!("could not create a tao window: {e}")),
    };
    let webview = match WebviewSurface::new(&window, runtime.handle(), web_sub, proxy) {
        Ok(s) => s,
        Err(e) => fail_setup(runtime, &format!("could not create the wry webview: {e}")),
    };

    // Headless: no window is open for this surface, so `start_renderer`
    // realizes the implicit view into a private hidden holding window — boot
    // IS the view's realization on Linux (L-F1/L-F2) — and nothing here ever
    // calls `attach_view`, which a headless-booted engine would refuse anyway:
    // the realized view can never move into a real window afterwards (L-F3),
    // so drivers that want a visible view must `open_window` before the
    // renderer starts. See the module docs on why headless is deliberate here.
    let flutter_surface = SurfaceId::from_raw(1);
    let mut backend = LinuxBackend::new(data_dir);
    if let Err(e) = backend.start_renderer(flutter_surface) {
        fail_setup(runtime, &format!("could not boot a Flutter engine: {e}"));
    }
    let Some(engine) = backend.engine(flutter_surface) else {
        fail_setup(
            runtime,
            "the backend reported a booted renderer but has no engine for it",
        );
    };
    let flutter = match FlutterSurface::new(engine, runtime.handle(), dart_sub) {
        Ok(s) => s,
        Err(e) => fail_setup(
            runtime,
            &format!("could not register the duet/rpc handler: {e}"),
        ),
    };
    println!(
        "[two_guests] both guests constructed: webview subscriber={web_sub:?}, \
         Flutter subscriber={dart_sub:?}"
    );

    let mut app = App {
        start: StdInstant::now(),
        step: Step::AwaitWebBoot,
        probe: Arc::new(Mutex::new(Probe::default())),
        webview: Some(webview),
        window: Some(window),
        flutter: Some(flutter),
        backend,
        flutter_surface,
        runtime: Some(runtime),
        web_sub,
        dart_sub,
        paths,
        dart_report: None,
        results: Results::default(),
        settle_left: 0,
        log_len_before_attack: 0,
        notes_for_web: 0,
        notes_for_dart: 0,
        notes_for_nobody: 0,
        push_errors: 0,
        timed_out_at: None,
    };

    event_loop.run(move |event, _target, control_flow| {
        drive(&mut app, event, control_flow);
    });
}

/// Parses every path this example needs, so a typo is one setup failure rather
/// than an `unwrap` somewhere deep in the sequence.
fn parse_paths() -> Result<Paths, String> {
    let parse = |s: &str| Path::parse(s).map_err(|e| format!("{s:?} must be a valid path: {e:?}"));
    Ok(Paths {
        from_js: parse(FROM_JS_PATH)?,
        from_dart: parse(FROM_DART_PATH)?,
        both: parse(BOTH_PATH)?,
        js_only: parse(JS_ONLY_PATH)?,
        dart_only: parse(DART_ONLY_PATH)?,
        dart_report: parse(DART_REPORT_PATH)?,
    })
}

/// Reports a setup failure as a FAIL line and exits non-zero, rather than
/// panicking: a machine that cannot create a window, a webview or a Flutter
/// engine is a finding about the environment, and it should read the same way
/// as any other failure in this example's output.
fn fail_setup(runtime: Runtime, reason: &str) -> ! {
    println!("FAIL: setup - {reason}");
    if let Err(e) = runtime.shutdown() {
        println!("[two_guests] runtime shutdown failed: {e}");
    }
    std::process::exit(1);
}

/// The event-loop body. [`DuetEvent::WebviewScript`] **is** the webview's reply
/// path (its IPC handler cannot hold the webview it answers through, since
/// `wry` installs the handler before `build_gtk` yields the `WebView`), and
/// [`DuetEvent::Notifications`] is the push path for **both** guests.
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
            match app.webview.as_ref() {
                // `deliver` rather than `eval`: the event names the surface its
                // reply was produced for, and the surface checks. With two
                // guests in this process — and, in an embedder with two
                // webviews, two surfaces reachable from this arm — a driver
                // that evaluated every script into whichever webview it
                // happened to hold would settle the wrong pending call and hand
                // one renderer another renderer's data.
                Some(surface) => {
                    if let Err(e) = surface.deliver(subscriber, &script) {
                        println!("[two_guests] evaluating a host script failed: {e}");
                    }
                }
                // Expected after teardown: the webview's own in-flight replies
                // have nowhere to land, which is what being torn down means.
                None => println!("[two_guests] dropped a reply for the torn-down webview"),
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

/// Hands **every** notification to **both** surfaces.
///
/// The fan-out is the test. Each surface's `push` compares the notification's
/// `subscriber` against its own and drops anything addressed elsewhere, so
/// routing the whole stream through both is what exercises that boundary
/// rather than sidestepping it with a call-site check — see the module docs.
/// The per-guest tallies below are bookkeeping for the report; nothing here
/// branches on them.
fn deliver_pushes(app: &mut App, batch: Vec<Notification>) {
    for note in batch {
        if note.subscriber == app.web_sub {
            app.notes_for_web += 1;
        } else if note.subscriber == app.dart_sub {
            app.notes_for_dart += 1;
        } else {
            app.notes_for_nobody += 1;
            println!(
                "[two_guests] a notification arrived for {:?}, which is neither guest",
                note.subscriber
            );
        }

        if let Some(surface) = app.webview.as_ref() {
            if let Err(e) = surface.push(&note) {
                app.push_errors += 1;
                println!("[two_guests] the webview surface refused a notification: {e}");
            }
        }
        if let Some(surface) = app.flutter.as_ref() {
            if let Err(e) = surface.push(&note) {
                app.push_errors += 1;
                println!("[two_guests] the Flutter surface refused a notification: {e}");
            }
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
    app.dart_report = read_dart_report(app);

    match app.step {
        Step::AwaitWebBoot => await_web_boot(app, &probe),
        Step::WebWrite => app.step = eval_stage(app, "set", &set_js(), Step::AwaitWebSetReply),
        Step::AwaitWebSetReply => await_web_replies(app, &probe, 1, Step::AwaitDartWrote),
        Step::AwaitDartWrote => await_dart_stage(app, "wrote", Step::WebCrossRead),
        Step::WebCrossRead => {
            app.step = eval_stage(app, "cross-read", &cross_read_js(), Step::AwaitCrossReads)
        }
        Step::AwaitCrossReads => await_cross_reads(app, &probe),
        Step::WebSubscribe => {
            app.step = eval_stage(app, "subscribe", &subscribe_js(), Step::AwaitArmed)
        }
        Step::AwaitArmed => await_armed(app, &probe),
        Step::WriteBoth => {
            rust_write(app, &app.paths.both, WROTE_BOTH);
            app.step = Step::AwaitBothPushes;
        }
        Step::AwaitBothPushes => await_pushes(app, &probe, 1, 1, Step::SettleBoth),
        Step::SettleBoth => settle_both(app, &probe),
        Step::WriteDartOnly => {
            rust_write(app, &app.paths.dart_only, WROTE_DART_ONLY);
            app.step = Step::AwaitDartOnlyPush;
        }
        Step::AwaitDartOnlyPush => await_pushes(app, &probe, 0, 2, Step::SettleDartOnly),
        Step::SettleDartOnly => settle_dart_only(app, &probe),
        Step::WriteJsOnly => {
            rust_write(app, &app.paths.js_only, WROTE_JS_ONLY);
            app.step = Step::AwaitJsOnlyPush;
        }
        Step::AwaitJsOnlyPush => await_pushes(app, &probe, 2, 0, Step::SettleJsOnly),
        Step::SettleJsOnly => settle_js_only(app, &probe),
        Step::WebAttack => web_attack(app, &probe),
        Step::AwaitAttackAnswered => await_attack_answered(app, &probe),
        Step::WriteAfterAttack => write_after_attack(app),
        Step::AwaitDartPushAfterAttack => {
            await_dart_delivery(app, WROTE_AFTER_ATTACK, Step::SettleAfterAttack)
        }
        Step::SettleAfterAttack => settle_after_attack(app, &probe),
        Step::TearDownWebview => tear_down_webview(app),
        Step::WriteAfterTeardown => {
            rust_write(app, &app.paths.dart_only, WROTE_AFTER_TEARDOWN);
            app.step = Step::AwaitDartPushAfterTeardown;
        }
        Step::AwaitDartPushAfterTeardown => await_dart_push_after_teardown(app),
        Step::Report => report_and_exit(app, &probe),
    }
    request_probe(app);
}

/// Copies the latest webview readback out from under its lock. A poisoned lock
/// means the callback panicked; treat that as "no data yet" rather than
/// propagating, so the deadline reports it instead of this turn panicking.
fn read_probe(app: &App) -> Probe {
    match app.probe.lock() {
        Ok(guard) => guard.clone(),
        Err(_) => Probe::default(),
    }
}

/// Asks the webview guest for a fresh readback; the callback lands on a later
/// turn. A no-op once the webview has been torn down.
fn request_probe(app: &App) {
    let Some(surface) = app.webview.as_ref() else {
        return;
    };
    let slot = Arc::clone(&app.probe);
    let outcome = surface.eval_with_callback(PROBE_JS, move |json| {
        if let Ok(mut guard) = slot.lock() {
            *guard = parse_probe(&json);
        }
    });
    if let Err(e) = outcome {
        println!("[two_guests] readback probe failed: {e}");
    }
}

/// Parses [`PROBE_JS`]'s return value. Total: anything unexpected becomes a
/// default [`Probe`] carrying the raw text, which the report prints.
fn parse_probe(json: &str) -> Probe {
    let parsed: serde_json::Value = serde_json::from_str(json).unwrap_or(serde_json::Value::Null);
    let string_at = |key: &str| parsed.get(key).and_then(|v| v.as_str()).map(str::to_string);
    let u64_at = |key: &str| parsed.get(key).and_then(|v| v.as_u64()).unwrap_or(0);
    Probe {
        ready: parsed
            .get("ready")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        log_len: u64_at("logLen"),
        push_len: u64_at("pushLen"),
        last_path: string_at("lastPath"),
        last_tag: string_at("lastTag"),
        last_value: string_at("lastValue"),
        last_subscriber: string_at("lastSubscriber"),
        last_subscription: string_at("lastSubscription"),
        cross_read: string_at("crossRead"),
        sub_both: string_at("subBoth"),
        sub_js_only: string_at("subJsOnly"),
        attacks: u64_at("attacks"),
        raw: json.to_string(),
    }
}

/// Reads the Dart guest's published report out of the shared store.
fn read_dart_report(app: &App) -> Option<Value> {
    let rt = app.runtime.as_ref()?;
    match rt.handle().get(&app.paths.dart_report) {
        Ok(value) => value,
        Err(e) => {
            println!("[two_guests] reading the Dart guest's report failed: {e}");
            None
        }
    }
}

/// A map field lookup on the Dart guest's report.
fn field<'a>(value: Option<&'a Value>, key: &str) -> Option<&'a Value> {
    match value? {
        Value::Map(entries) => entries.get(key),
        _ => None,
    }
}

/// The Dart guest's current `stage`, if it has published one.
fn dart_stage(app: &App) -> Option<&str> {
    match field(app.dart_report.as_ref(), "stage")? {
        Value::Str(s) => Some(s.as_str()),
        _ => None,
    }
}

/// How far along the Dart guest's sequence a stage name is.
///
/// Ranked rather than compared for equality so a driver that arrives a turn
/// late does not miss the transition it was waiting for. An unrecognised
/// stage — including `crossReadTimedOut`, the fixture's own give-up marker —
/// ranks 0 and therefore never satisfies a wait, so the deadline reports it
/// with the guest's last report attached rather than the run quietly
/// continuing on a guest that gave up.
fn rank_of(stage: Option<&str>) -> u8 {
    match stage {
        Some("wrote") => 1,
        Some("crossRead") => 2,
        Some("armed") => 3,
        Some("done") => 4,
        _ => 0,
    }
}

/// An `i64` field of the Dart guest's report, or `None` if it has not
/// published one.
fn dart_int(app: &App, key: &str) -> Option<i64> {
    match field(app.dart_report.as_ref(), key)? {
        Value::Int(n) => Some(*n),
        _ => None,
    }
}

/// How many pushes the Dart guest has reported; 0 if it has not said.
fn dart_pushes(app: &App) -> i64 {
    dart_int(app, "pushes").unwrap_or(0)
}

/// The path the Dart guest's most recent push carried.
fn dart_push_path(app: &App) -> Option<&str> {
    match field(app.dart_report.as_ref(), "pushPath")? {
        Value::Str(s) => Some(s.as_str()),
        _ => None,
    }
}

/// Waits for the webview's bootstrap script to define `window.__duet`.
fn await_web_boot(app: &mut App, probe: &Probe) {
    if !probe.ready {
        return;
    }
    println!(
        "[two_guests] the webview guest booted after {:.2}s: {}",
        app.start.elapsed().as_secs_f64(),
        probe.raw
    );
    app.step = Step::WebWrite;
}

/// Evaluates one webview-side call, reporting an eval failure loudly. Returns
/// the stage to move to either way — a failed eval still has to reach the
/// report rather than stalling here.
fn eval_stage(app: &App, what: &str, script: &str, next: Step) -> Step {
    match app.webview.as_ref() {
        Some(surface) => match surface.eval(script) {
            Ok(()) => println!("[two_guests] evaluated the webview guest's {what} call"),
            Err(e) => {
                println!("[two_guests] evaluating the webview guest's {what} call failed: {e}")
            }
        },
        None => println!("[two_guests] {what} skipped: the webview is already torn down"),
    }
    next
}

/// Waits until the webview guest has received `wanted` responses in total.
fn await_web_replies(app: &mut App, probe: &Probe, wanted: u64, next: Step) {
    if probe.log_len < wanted {
        return;
    }
    println!(
        "[two_guests] the webview guest has received {} response(s) after {:.2}s",
        probe.log_len,
        app.start.elapsed().as_secs_f64()
    );
    app.step = next;
}

/// Waits until the Dart guest reports it has reached `stage` or later.
fn await_dart_stage(app: &mut App, stage: &str, next: Step) {
    if rank_of(dart_stage(app)) < rank_of(Some(stage)) {
        return;
    }
    println!(
        "[two_guests] the Dart guest reached stage {:?} after {:.2}s",
        dart_stage(app).unwrap_or("<none>"),
        app.start.elapsed().as_secs_f64()
    );
    app.step = next;
}

/// Claim 1: each guest read the *other* guest's key, and reported the exact
/// value it saw.
///
/// Both halves are recorded as soon as both guests have published something,
/// pass or fail — a mismatch must be reported, not waited on forever.
fn await_cross_reads(app: &mut App, probe: &Probe) {
    let dart_saw = field(app.dart_report.as_ref(), "crossRead").cloned();
    let (Some(web_saw), Some(dart_saw)) = (probe.cross_read.as_deref(), dart_saw) else {
        return;
    };
    // The fixture publishes `crossRead` as null until it has actually read the
    // other guest's key, so null means "not yet", not "read nothing".
    if dart_saw == Value::Null {
        return;
    }

    let want_web = format!("i:{DART_WRITES}");
    app.results.webview_sees_the_dart_write = Some((
        web_saw == want_web,
        format!(
            "the webview guest read {FROM_DART_PATH} = {web_saw:?} (want {want_web:?}, \
             written by the Dart guest)"
        ),
    ));

    let want_dart = Value::Int(JS_WRITES);
    app.results.dart_sees_the_webviews_write = Some((
        dart_saw == want_dart,
        format!(
            "the Dart guest read {FROM_JS_PATH} = {dart_saw:?} (want {want_dart:?}, \
             written by the webview guest)"
        ),
    ));
    if let Some((_, detail)) = &app.results.webview_sees_the_dart_write {
        println!("[two_guests] {detail}");
    }
    if let Some((_, detail)) = &app.results.dart_sees_the_webviews_write {
        println!("[two_guests] {detail}");
    }

    // Both guests reached the host and wrote. Read both keys back from Rust
    // too: the two cross-reads above are each guest's word for it, and the
    // host's own view of the store is the third, independent one. This is the
    // claim `webview_state.rs` and `flutter_state.rs` each make for one guest,
    // made here for both at once against a single store.
    let host_js = host_read(app, &app.paths.from_js);
    let host_dart = host_read(app, &app.paths.from_dart);
    app.results.both_guests_booted = Some((
        host_js == Some(Value::Int(JS_WRITES)) && host_dart == Some(Value::Int(DART_WRITES)),
        format!(
            "a wry webview and a headless FlutterEngine both reached the host within \
             {:.2}s; Rust reads {FROM_JS_PATH} = {host_js:?} and {FROM_DART_PATH} = \
             {host_dart:?} (want Int({JS_WRITES}) and Int({DART_WRITES}))",
            app.start.elapsed().as_secs_f64()
        ),
    ));
    if let Some((_, detail)) = &app.results.both_guests_booted {
        println!("[two_guests] {detail}");
    }
    app.step = Step::WebSubscribe;
}

/// Reads a path from Rust's own side of the store, for the claims that must
/// not rest solely on what a guest reported.
fn host_read(app: &App, path: &Path) -> Option<Value> {
    let rt = app.runtime.as_ref()?;
    match rt.handle().get(path) {
        Ok(value) => value,
        Err(e) => {
            println!("[two_guests] reading {path} from Rust failed: {e}");
            None
        }
    }
}

/// Waits until both guests report they are subscribed to the paths they need.
fn await_armed(app: &mut App, probe: &Probe) {
    let web_ready = probe.sub_both.is_some() && probe.sub_js_only.is_some();
    if !web_ready || rank_of(dart_stage(app)) < rank_of(Some("armed")) {
        return;
    }
    println!(
        "[two_guests] both guests armed after {:.2}s: webview both={:?} jsOnly={:?}; \
         Dart both={:?} dartOnly={:?}",
        app.start.elapsed().as_secs_f64(),
        probe.sub_both,
        probe.sub_js_only,
        dart_int(app, "subBoth"),
        dart_int(app, "subDartOnly"),
    );
    app.step = Step::WriteBoth;
}

/// Writes `value` at `path` from Rust. The store notifies every subscriber
/// watching it, which [`ProxySink`] marshals onto this thread as
/// [`DuetEvent::Notifications`].
///
/// Takes `&App` and leaves the stage transition to the caller, so a stage that
/// needs two writes (see [`write_after_attack`]) can make both before moving
/// on.
fn rust_write(app: &App, path: &Path, value: i64) {
    match app.runtime.as_ref() {
        Some(rt) => match rt.handle().set(path, Value::Int(value)) {
            Ok(()) => println!("[two_guests] Rust wrote {path} = Int({value})"),
            Err(e) => println!("[two_guests] Rust write of {path} failed: {e}"),
        },
        None => println!("[two_guests] Rust write skipped: the runtime is already shut down"),
    }
}

/// Waits until each guest has reported at least the given number of pushes.
/// A `0` means "do not wait on this guest" — used by the stages whose whole
/// point is that one guest must **not** receive anything.
fn await_pushes(app: &mut App, probe: &Probe, want_web: u64, want_dart: i64, next: Step) {
    if probe.push_len < want_web || dart_pushes(app) < want_dart {
        return;
    }
    println!(
        "[two_guests] push counts after {:.2}s: webview={}, Dart={}",
        app.start.elapsed().as_secs_f64(),
        probe.push_len,
        dart_pushes(app)
    );
    app.settle_left = SETTLE_TURNS;
    app.step = next;
}

/// Counts down the settle window, returning whether it has elapsed.
fn settled(app: &mut App) -> bool {
    app.settle_left = app.settle_left.saturating_sub(1);
    app.settle_left == 0
}

/// Claim 2, first half: one write to a shared path produced **exactly one**
/// push for each guest, and each push carried **that guest's own**
/// subscription id.
fn settle_both(app: &mut App, probe: &Probe) {
    if !settled(app) {
        return;
    }
    let dart = dart_pushes(app);
    // Counts *and* payloads: "exactly one push each" says nothing about what
    // arrived, and a guest handed the wrong write would still count as one.
    let web_payload = format!(
        "{}={{t:{},v:{}}}",
        probe.last_path.as_deref().unwrap_or("<none>"),
        probe.last_tag.as_deref().unwrap_or("<none>"),
        probe.last_value.as_deref().unwrap_or("<none>")
    );
    let web_payload_ok = probe.last_path.as_deref() == Some(BOTH_PATH)
        && probe.last_tag.as_deref() == Some("i")
        && probe.last_value.as_deref() == Some(WROTE_BOTH.to_string().as_str());
    let dart_path = dart_push_path(app).unwrap_or("<none>").to_string();
    let dart_value = field(app.dart_report.as_ref(), "pushValue").cloned();
    let dart_payload_ok = dart_path == BOTH_PATH && dart_value == Some(Value::Int(WROTE_BOTH));
    app.results.shared_write_pushed_once_to_each = Some((
        probe.push_len == 1 && dart == 1 && web_payload_ok && dart_payload_ok,
        format!(
            "one write of Int({WROTE_BOTH}) at {BOTH_PATH:?} produced {} push(es) for the \
             webview guest (carrying {web_payload}) and {dart} for the Dart guest (carrying \
             {dart_path}={dart_value:?}); want exactly 1 each, both carrying that write",
            probe.push_len
        ),
    ));

    let web_own = probe.sub_both.clone().unwrap_or_default();
    let web_got = probe.last_subscription.clone().unwrap_or_default();
    let web_subscriber = probe.last_subscriber.clone().unwrap_or_default();
    let dart_own = dart_int(app, "subBoth").unwrap_or(-1);
    let dart_got = dart_int(app, "pushSubscription").unwrap_or(-1);
    let dart_subscriber = dart_int(app, "pushSubscriber").unwrap_or(-1);
    let web_ok =
        !web_own.is_empty() && web_own == web_got && web_subscriber == app.web_sub.0.to_string();
    let dart_ok = dart_own >= 0
        && dart_own == dart_got
        && u64::try_from(dart_subscriber).is_ok_and(|id| id == app.dart_sub.0);
    app.results.each_push_carried_its_own_subscription = Some((
        web_ok && dart_ok,
        format!(
            "the webview's push carried subscriber={web_subscriber:?} subscription={web_got:?} \
             (its own: subscriber={}, subscription={web_own:?}); the Dart guest's carried \
             subscriber={dart_subscriber} subscription={dart_got} (its own: subscriber={}, \
             subscription={dart_own})",
            app.web_sub.0, app.dart_sub.0
        ),
    ));
    app.step = Step::WriteDartOnly;
}

/// Claim 2, second half (one direction): a write only the Dart guest watches
/// must not have moved the webview's push count.
fn settle_dart_only(app: &mut App, probe: &Probe) {
    if !settled(app) {
        return;
    }
    let dart = dart_pushes(app);
    app.results.dart_only_write_stayed_away_from_the_webview = Some((
        probe.push_len == 1 && dart == 2,
        format!(
            "after a write at {DART_ONLY_PATH:?}, which only the Dart guest watches: the \
             webview guest has {} push(es) (want 1, unchanged) and the Dart guest has \
             {dart} (want 2)",
            probe.push_len
        ),
    ));
    app.step = Step::WriteJsOnly;
}

/// Claim 2, second half (the other direction): a write only the webview
/// watches must not have moved the Dart guest's push count.
fn settle_js_only(app: &mut App, probe: &Probe) {
    if !settled(app) {
        return;
    }
    let dart = dart_pushes(app);
    app.results.js_only_write_stayed_away_from_flutter = Some((
        probe.push_len == 2 && dart == 2,
        format!(
            "after a write at {JS_ONLY_PATH:?}, which only the webview guest watches: the \
             Dart guest has {dart} push(es) (want 2, unchanged) and the webview guest has \
             {} (want 2)",
            probe.push_len
        ),
    ));
    app.step = Step::WebAttack;
}

/// Fires the hostile `unsubscribe` sweep from the webview guest.
fn web_attack(app: &mut App, probe: &Probe) {
    // Recorded as a baseline rather than an absolute, so the "the host
    // answered all of them" control below measures a delta. Safe to read from
    // the (one turn stale) probe here: every legitimate reply landed stages
    // ago — the run only reached this point because both subscribe replies
    // were observed — and no IPC happens in between. The readback probe itself
    // uses `evaluate_script_with_callback`, which is not IPC and never touches
    // this counter.
    app.log_len_before_attack = probe.log_len;
    println!(
        "[two_guests] the webview guest is about to try unsubscribing ids \
         {ATTACK_ID_MIN}..={ATTACK_ID_MAX} (it has received {} response(s) so far)",
        probe.log_len
    );
    app.step = eval_stage(
        app,
        "hostile unsubscribe",
        &attack_js(),
        Step::AwaitAttackAnswered,
    );
}

/// The control for claim 3: the host answered every hostile request.
///
/// Without this, "the Dart guest survived" could mean the attack never
/// arrived — a webview whose IPC silently dropped these would look identical
/// to a host that correctly refused them.
fn await_attack_answered(app: &mut App, probe: &Probe) {
    let answered = probe.log_len.saturating_sub(app.log_len_before_attack);
    if answered < ATTACK_COUNT {
        return;
    }
    app.results.hostile_unsubscribes_reached_the_host = Some((
        probe.attacks == ATTACK_COUNT && answered == ATTACK_COUNT,
        format!(
            "the webview guest posted {} hostile unsubscribe(s) for ids \
             {ATTACK_ID_MIN}..={ATTACK_ID_MAX} and the host answered {answered} of them \
             (want {ATTACK_COUNT} each)",
            probe.attacks
        ),
    ));
    if let Some((_, detail)) = &app.results.hostile_unsubscribes_reached_the_host {
        println!("[two_guests] {detail}");
    }
    app.step = Step::WriteAfterAttack;
}

/// Writes to both single-guest paths after the attack.
///
/// The webview's write goes first so that the Dart guest's push is the last
/// one either guest could receive, which is what [`await_dart_delivery`]
/// asserts on. If the webview's write leaked to the Dart guest, the count
/// checked in [`settle_after_attack`] catches it.
fn write_after_attack(app: &mut App) {
    rust_write(app, &app.paths.js_only, WROTE_JS_AFTER_ATTACK);
    rust_write(app, &app.paths.dart_only, WROTE_AFTER_ATTACK);
    app.step = Step::AwaitDartPushAfterAttack;
}

/// Waits for the Dart guest to report a push carrying **exactly** `value` at
/// [`DART_ONLY_PATH`].
///
/// Delivery, not a count: a check that only compared numbers could pass
/// because nothing was ever written. The push has to actually arrive, carrying
/// the value Rust put there.
fn await_dart_delivery(app: &mut App, value: i64, next: Step) {
    let path_ok = dart_push_path(app) == Some(DART_ONLY_PATH);
    let value_ok = field(app.dart_report.as_ref(), "pushValue") == Some(&Value::Int(value));
    if !path_ok || !value_ok {
        return;
    }
    println!(
        "[two_guests] the Dart guest received {DART_ONLY_PATH} = Int({value}) after {:.2}s \
         ({} push(es) total)",
        app.start.elapsed().as_secs_f64(),
        dart_pushes(app)
    );
    app.settle_left = SETTLE_TURNS;
    app.step = next;
}

/// Claim 3 and its positive control.
fn settle_after_attack(app: &mut App, probe: &Probe) {
    if !settled(app) {
        return;
    }
    let dart = dart_pushes(app);
    app.results.flutter_survived_the_webviews_attack = Some((
        dart == 3,
        format!(
            "after the webview tried to unsubscribe ids {ATTACK_ID_MIN}..={ATTACK_ID_MAX}, \
             the Dart guest received {DART_ONLY_PATH} = Int({WROTE_AFTER_ATTACK}) — push \
             number {dart} (want 3)"
        ),
    ));
    app.results.the_attacker_unsubscribed_itself = Some((
        probe.push_len == 2,
        format!(
            "the attacker's own subscriptions were among the ids it cancelled, so a later \
             write at {JS_ONLY_PATH:?} must not reach it: the webview guest has {} push(es) \
             (want 2, unchanged)",
            probe.push_len
        ),
    ));
    app.step = Step::TearDownWebview;
}

/// Drops the webview surface and the window hosting it.
///
/// The surface first: it owns the `wry` `WebView`, a WebKitGTK widget packed
/// into the default vbox of the window it was built in (L-F7), and dropping
/// the window out from under it would be the reverse of how it was built.
fn tear_down_webview(app: &mut App) {
    let had_surface = app.webview.take().is_some();
    let had_window = app.window.take().is_some();
    println!(
        "[two_guests] tore the webview guest down (surface dropped: {had_surface}, \
         window dropped: {had_window}); the Flutter guest is untouched"
    );
    app.step = Step::WriteAfterTeardown;
}

/// Claim 4, first half: the surviving guest still receives its pushes.
fn await_dart_push_after_teardown(app: &mut App) {
    let path_ok = dart_push_path(app) == Some(DART_ONLY_PATH);
    let value_ok =
        field(app.dart_report.as_ref(), "pushValue") == Some(&Value::Int(WROTE_AFTER_TEARDOWN));
    if !path_ok || !value_ok {
        return;
    }
    let dart = dart_pushes(app);
    app.results.flutter_survived_the_webviews_teardown = Some((
        dart == 4,
        format!(
            "with the webview surface dropped, the Dart guest received {DART_ONLY_PATH} = \
             Int({WROTE_AFTER_TEARDOWN}) — its {dart}th push (want 4) — after {:.2}s",
            app.start.elapsed().as_secs_f64()
        ),
    ));
    if let Some((_, detail)) = &app.results.flutter_survived_the_webviews_teardown {
        println!("[two_guests] {detail}");
    }
    app.step = Step::Report;
}

/// Tears the Flutter side down, shuts the runtime down, prints every claim as
/// a PASS or FAIL line, and exits — non-zero if anything failed.
///
/// Exiting explicitly rather than returning: `tao::EventLoop::run` never
/// returns, so this is the only place a process exit code can be set.
fn report_and_exit(app: &mut App, probe: &Probe) -> ! {
    // Teardown order is not interchangeable: the surface must unregister its
    // channel handler (its `Drop`) before the engine shuts down, because the
    // messenger holds the handler's `user_data` pointer past registration —
    // and `destroy_renderer` *forces* the engine's dispose (see the crate's
    // engine docs), after which there is no live channel table to
    // unregister from. The surface's own references keep its send path
    // memory-safe, nothing more.
    let had_flutter = app.flutter.take().is_some();
    if let Err(e) = app.backend.destroy_renderer(app.flutter_surface) {
        println!("[two_guests] destroying the Flutter renderer failed: {e}");
    }
    println!("[two_guests] tore the Flutter guest down (surface dropped: {had_flutter})");

    // Claim 4's second half. Both guests are gone; a runtime that could not
    // shut down cleanly now would mean one of them left something behind.
    if let Some(rt) = app.runtime.take() {
        app.results.runtime_shutdown_was_clean = Some(match rt.shutdown() {
            Ok(()) => (
                true,
                "the core thread shut down cleanly after both guests were torn down".to_string(),
            ),
            Err(e) => (false, format!("Runtime::shutdown reported {e}")),
        });
    }

    println!();
    println!("=== Duet two-guest isolation report ===");
    if let Some(step) = app.timed_out_at {
        println!("TIMED OUT after {DEADLINE:?} while at stage {step:?}");
    }
    println!("webview guest, last readback: {}", probe.raw);
    println!("Dart guest, last report: {:?}", app.dart_report);
    println!(
        "notifications addressed to the webview: {}, to the Flutter guest: {}, to neither: {}",
        app.notes_for_web, app.notes_for_dart, app.notes_for_nobody
    );
    println!(
        "every notification was offered to both surfaces; surface push errors: {}",
        app.push_errors
    );
    println!();

    let r = &app.results;
    let checks = [
        (
            "a wry webview and a Flutter engine ran as two guests at once",
            &r.both_guests_booted,
        ),
        (
            "the Dart guest read the value the webview guest wrote",
            &r.dart_sees_the_webviews_write,
        ),
        (
            "the webview guest read the value the Dart guest wrote",
            &r.webview_sees_the_dart_write,
        ),
        (
            "one write to a shared path pushed exactly once to each guest",
            &r.shared_write_pushed_once_to_each,
        ),
        (
            "each push carried its own guest's subscription, not the other's",
            &r.each_push_carried_its_own_subscription,
        ),
        (
            "a write only Flutter watches never reached the webview",
            &r.dart_only_write_stayed_away_from_the_webview,
        ),
        (
            "a write only the webview watches never reached Flutter",
            &r.js_only_write_stayed_away_from_flutter,
        ),
        (
            "every hostile unsubscribe actually reached the host",
            &r.hostile_unsubscribes_reached_the_host,
        ),
        (
            "the webview could not unsubscribe the Flutter guest",
            &r.flutter_survived_the_webviews_attack,
        ),
        (
            "control: the webview did unsubscribe itself",
            &r.the_attacker_unsubscribed_itself,
        ),
        (
            "tearing the webview down left the Flutter guest working",
            &r.flutter_survived_the_webviews_teardown,
        ),
        (
            "the runtime shut down cleanly with both guests gone",
            &r.runtime_shutdown_was_clean,
        ),
    ];
    let failures = checks
        .iter()
        .filter(|(label, outcome)| !print_check(label, outcome))
        .count();

    println!();
    if failures == 0 {
        println!("ALL PASS: two live guests share one store and neither can disturb the other");
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
