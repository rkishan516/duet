//! One function per act of the tour.
//!
//! Each returns after doing at most one turn's worth of work: a wait either
//! finds what it is waiting for and moves on, or does nothing and is called
//! again 50 ms later. Nothing here blocks and nothing here sleeps.

use std::time::Instant;

use duet::Reading;
use duet_host::WindowBackend;
use tao::event_loop::EventLoopWindowTarget;

use crate::tour::backend::DuetEvent;

use duet_showcase::state::HostNote;

use crate::tour::fields::{int, lines, text};
use crate::tour::guests;
use crate::tour::rss::Sample;
use crate::tour::{AWAY_LINE, MIN_RECLAIM_PERCENT, SETTLE_TURNS, Step, Tour};

/// The label of each RSS sample, in the order they are taken.
const WEB_ONLY: &str = "webview guest live";
const BOTH_LIVE: &str = "both guests live";
const TORN_DOWN: &str = "Flutter guest torn down";
const REBOOTED: &str = "Flutter guest booted again";

/// Publishes what the host is doing, into the store and onto the terminal.
///
/// Into the store as well as onto the terminal because both guests watch `host`
/// and draw it: on a machine with a display, the narration is on screen next to
/// the values it is talking about.
fn narrate(tour: &mut Tour, act: &str, detail: &str) {
    println!("[showcase] {:>6.2}s  {act} — {detail}", tour.elapsed());
    let note = HostNote {
        act: act.to_string(),
        detail: detail.to_string(),
    };
    if let Err(e) = tour.fields.host.set(&note) {
        println!("[showcase] publishing the host's note failed: {e}");
    }
}

/// Waits for the `wry` bootstrap page to define `window.__duet`.
pub fn await_web_boot(tour: &mut Tour, probe: &guests::WebProbe) {
    if !probe.bootstrapped {
        return;
    }
    tour.samples.push(Sample::take(WEB_ONLY));
    narrate(
        tour,
        "act 1: attach",
        "the wry webview booted; evaluating the showcase bundle into it",
    );
    tour.step = Step::MountWebGuest;
}

/// Evaluates the bundled TypeScript guest into the webview.
///
/// `WebviewSurface` builds its page from `duet_webview::bootstrap::BOOTSTRAP_HTML`
/// and offers no way to supply another one, so a real page has to arrive as a
/// script and build its own DOM. See the README.
pub fn mount_web_guest(tour: &mut Tour) {
    match tour.webview.as_ref() {
        Some(surface) => {
            if let Err(e) = surface.eval(&tour.bundle) {
                println!("[showcase] evaluating the webview bundle failed: {e}");
            }
        }
        None => println!("[showcase] no webview to evaluate the bundle into"),
    }
    tour.step = Step::AwaitWebMounted;
}

/// Waits for the bundle to report that its panel is up.
pub fn await_web_mounted(tour: &mut Tour, probe: &guests::WebProbe) {
    if probe.trouble != tour.web_trouble {
        tour.web_trouble.clone_from(&probe.trouble);
        println!(
            "[showcase] the webview guest reported trouble: {}",
            probe.trouble
        );
    }
    if !probe.mounted {
        return;
    }
    narrate(
        tour,
        "act 1: attach",
        "the webview guest is up; booting a Flutter engine alongside it",
    );
    tour.step = Step::BootFlutter;
}

/// Opens a window, boots a Flutter engine, and attaches its view.
pub fn boot_flutter(tour: &mut Tour, target: &EventLoopWindowTarget<DuetEvent>) {
    if let Err(reason) = boot_flutter_guest(tour, target) {
        println!("FAIL: {reason}");
        tour.exit_code = 1;
        tour.step = Step::Report;
        return;
    }
    tour.step = Step::AwaitGuestsReady;
}

/// The whole Flutter boot, used both for the first one and for the reboot.
fn boot_flutter_guest(
    tour: &mut Tour,
    target: &EventLoopWindowTarget<DuetEvent>,
) -> Result<(), String> {
    tour.backend
        .open_window(tour.flutter_id, target, "Duet showcase — Flutter guest")
        .map_err(|e| format!("opening the Flutter guest's window failed: {e}"))?;
    let surface = guests::boot_flutter(
        &mut tour.backend,
        tour.flutter_id,
        tour.store.clone(),
        tour.flutter_sub,
    )?;
    // A real view, not a headless engine: this guest has a widget tree, and the
    // hot-reload story below depends on frames actually being produced.
    tour.backend
        .attach_view(tour.flutter_id)
        .map_err(|e| format!("attaching the Flutter view failed: {e}"))?;
    tour.flutter = Some(surface);
    Ok(())
}

/// Waits until both guests have finished their opening moves.
///
/// `status == "ready"` is written last by each guest, after its watchers are
/// armed and its commands have answered, so this one field is a complete
/// readiness signal rather than a partial one.
pub fn await_guests_ready(tour: &mut Tour) {
    let flutter = text(&tour.fields.flutter.status);
    let web = text(&tour.fields.web.status);
    if flutter != "ready" || web != "ready" {
        return;
    }
    narrate(
        tour,
        "act 1: attach",
        "both guests report ready; settling before checking what they saw",
    );
    tour.settle = SETTLE_TURNS;
    tour.step = Step::CheckOpening;
}

/// Checks acts 1 to 3: both attached, each sees the other, and both invoked.
pub fn check_opening(tour: &mut Tour) {
    if !tour.settled() {
        return;
    }
    println!();
    println!("=== acts 1-3: shared state, subscriptions, commands ===");

    let elapsed = tour.elapsed();
    tour.results.record(
        "both_guests_attached",
        true,
        format!(
            "a wry webview and a FlutterEngine both reached the host and finished their opening \
             moves within {elapsed:.2}s"
        ),
    );

    // Compared against what is actually in the store, not against a constant
    // this file guesses: the claim is that each guest's watcher delivered the
    // *other guest's* write, so the source of truth is the peer's own field.
    let web_note = text(&tour.fields.web.note);
    let flutter_note = text(&tour.fields.flutter.note);
    let flutter_saw = text(&tour.fields.flutter.saw_peer_note);
    let web_saw = text(&tour.fields.web.saw_peer_note);
    tour.results.record(
        "each_guest_sees_the_others_write",
        !web_note.is_empty()
            && !flutter_note.is_empty()
            && flutter_saw == web_note
            && web_saw == flutter_note,
        format!(
            "the Flutter guest's watcher reported web.note = {flutter_saw:?} (the webview wrote \
             {web_note:?}); the webview guest's watcher reported flutter.note = {web_saw:?} (the \
             Flutter guest wrote {flutter_note:?})"
        ),
    );

    let document = lines(&tour.fields.lines);
    let flutter_lines = int(&tour.fields.flutter.saw_lines);
    let web_lines = int(&tour.fields.web.saw_lines);
    let distinct = document.len() == 2 && document.first() != document.get(1);
    tour.results.record(
        "one_document_two_writers",
        distinct && flutter_lines == Some(2) && web_lines == Some(2),
        format!(
            "document.lines holds {} line(s) {document:?}; the Flutter guest's typed watcher last \
             saw {flutter_lines:?} and the webview guest's saw {web_lines:?} (want 2 distinct \
             lines and 2 seen by each)",
            document.len()
        ),
    );

    let flutter_returned = text(&tour.fields.flutter.returned);
    let web_returned = text(&tour.fields.web.returned);
    tour.results.record(
        "a_command_returned_for_both_guests",
        flutter_returned.contains("returned") && web_returned.contains("returned"),
        format!("Flutter: {flutter_returned:?}; WebView: {web_returned:?}"),
    );

    let flutter_raised = text(&tour.fields.flutter.raised);
    let web_raised = text(&tour.fields.web.raised);
    tour.results.record(
        "a_command_raised_for_both_guests",
        flutter_raised.contains("raised") && web_raised.contains("raised"),
        format!("Flutter: {flutter_raised:?}; WebView: {web_raised:?}"),
    );

    tour.step = if tour.linger.is_zero() {
        Step::SampleLive
    } else {
        arm_linger(tour)
    };
}

/// Announces the hot-reload window and starts its clock.
///
/// Deliberately *before* the teardown act, not after it. `duet dev` finds the
/// Dart VM service by reading the first announcement out of the host's stdout,
/// and this host boots a second engine later on with a second, different VM
/// service — so the only engine `duet dev` can reload is the first one, and the
/// only place to pause for a reload is while that one is alive.
fn arm_linger(tour: &mut Tour) -> Step {
    println!();
    println!(
        "=== lingering for {}s: both guests are live and hot reload is armed ===",
        tour.linger.as_secs()
    );
    println!(
        "    Edit kFlutterNote in examples/showcase/flutter/lib/src/showcase_app.dart and save."
    );
    tour.linger_until = Some(Instant::now() + tour.linger);
    narrate(
        tour,
        "hot reload",
        "edit kFlutterNote in the Flutter guest and save; duet dev will reload it",
    );
    Step::Linger
}

/// Samples RSS with both guests live.
pub fn sample_live(tour: &mut Tour) {
    tour.samples.push(Sample::take(BOTH_LIVE));
    tour.lines_at_teardown = lines(&tour.fields.lines).len();
    println!();
    println!("=== act 4: teardown and reclaim ===");
    narrate(
        tour,
        "act 4: teardown",
        "tearing the Flutter guest down: handler, engine, window",
    );
    tour.step = Step::TearDownFlutter;
}

/// Tears the Flutter guest down and wipes everything it published.
pub fn tear_down_flutter(tour: &mut Tour) {
    // Drop the guest's store identity before the surface: its subscriptions
    // outlive the renderer otherwise, and the store would keep addressing
    // notifications to a subscriber nothing can deliver to.
    match tour.store.drop_subscriber(tour.flutter_sub) {
        Ok(dropped) => {
            println!("[showcase] dropped {dropped} subscription(s) for the Flutter guest")
        }
        Err(e) => println!("[showcase] dropping the Flutter guest's subscriptions failed: {e}"),
    }

    let handler = tour.flutter.take();
    if let Err(reason) = guests::tear_down_flutter(&mut tour.backend, tour.flutter_id, handler) {
        println!("FAIL: {reason}");
        tour.exit_code = 1;
    }
    if let Err(reason) = tour.fields.flutter.clear("torn down") {
        println!("FAIL: {reason}");
        tour.exit_code = 1;
    }
    narrate(
        tour,
        "act 4: teardown",
        "the Flutter guest no longer exists; its published state has been wiped",
    );
    tour.settle = SETTLE_TURNS;
    tour.step = Step::SettleTeardown;
}

/// Gives the allocator a moment before measuring.
pub fn settle_teardown(tour: &mut Tour) {
    if !tour.settled() {
        return;
    }
    tour.step = Step::SampleAfterTeardown;
}

/// Measures what teardown gave back.
pub fn sample_after_teardown(tour: &mut Tour) {
    tour.samples.push(Sample::take(TORN_DOWN));
    let engine_cost = delta(tour, BOTH_LIVE, WEB_ONLY);
    let reclaimed = delta(tour, TORN_DOWN, BOTH_LIVE).map(|d| -d);

    let (passed, share) = match (engine_cost, reclaimed) {
        (Some(cost), Some(back)) if cost > 0 => (
            back > 0 && back.saturating_mul(100) >= MIN_RECLAIM_PERCENT.saturating_mul(cost),
            format!("{}%", back.saturating_mul(100) / cost),
        ),
        _ => (false, "unmeasurable".to_string()),
    };
    tour.results.record(
        "tearing_the_flutter_guest_down_reclaims_memory",
        passed,
        format!(
            "booting the Flutter guest cost {engine_cost:?} kB; tearing it down gave back \
             {reclaimed:?} kB ({share} of it; the floor is {MIN_RECLAIM_PERCENT}%)"
        ),
    );

    println!();
    println!("=== act 5: the store outlives the guest ===");
    tour.step = Step::WriteWhileAway;
}

/// Appends a line while the Flutter guest does not exist.
pub fn write_while_away(tour: &mut Tour) {
    let mut document = lines(&tour.fields.lines);
    document.push(AWAY_LINE.to_string());
    if let Err(e) = tour.fields.lines.set(&document) {
        println!("[showcase] writing the away line failed: {e}");
    }
    if let Err(e) = tour
        .fields
        .title
        .set(&"written while one guest did not exist".to_string())
    {
        println!("[showcase] writing the title failed: {e}");
    }
    narrate(
        tour,
        "act 5: the store outlives the guest",
        "the host appended a line with only one guest attached",
    );
    tour.step = Step::AwaitWebSawIt;
}

/// Checks that the surviving guest noticed.
pub fn await_web_saw_it(tour: &mut Tour) {
    let want = i64::try_from(tour.lines_at_teardown + 1).unwrap_or(i64::MAX);
    let seen = int(&tour.fields.web.saw_lines);
    if seen != Some(want) {
        return;
    }
    tour.results.record(
        "the_surviving_guest_was_undisturbed",
        true,
        format!(
            "with the Flutter guest gone, the webview guest's typed watcher still fired: it \
             reports {seen:?} line(s) (want {want})"
        ),
    );
    println!();
    println!("=== act 6: boot it again, state intact ===");
    narrate(
        tour,
        "act 6: restore",
        "booting a second Flutter engine for the same surface",
    );
    tour.step = Step::RebootFlutter;
}

/// Boots a second Flutter engine for the same surface.
pub fn reboot_flutter(tour: &mut Tour, target: &EventLoopWindowTarget<DuetEvent>) {
    // A fresh subscriber id, because the old one was dropped along with the
    // guest. Reusing it would work, and would also quietly hide the fact that
    // the store's identity for a guest belongs to the host, not to the renderer.
    tour.flutter_sub = tour.store.next_subscriber_id();
    if let Err(reason) = boot_flutter_guest(tour, target) {
        println!("FAIL: {reason}");
        tour.exit_code = 1;
        tour.step = Step::Report;
        return;
    }
    tour.step = Step::AwaitFlutterBack;
}

/// Waits for the new Flutter guest to finish its opening moves.
pub fn await_flutter_back(tour: &mut Tour) {
    if text(&tour.fields.flutter.status) != "ready" {
        return;
    }
    narrate(
        tour,
        "act 6: restore",
        "the second Flutter guest is ready; settling before checking what it found",
    );
    tour.settle = SETTLE_TURNS;
    tour.step = Step::CheckRestored;
}

/// Checks that the rebooted guest rediscovered state it never saw written.
pub fn check_restored(tour: &mut Tour) {
    if !tour.settled() {
        return;
    }
    let document = lines(&tour.fields.lines);
    let seen = int(&tour.fields.flutter.saw_lines);
    let want = i64::try_from(document.len()).unwrap_or(i64::MAX);
    let has_away = document.iter().any(|line| line == AWAY_LINE);
    let peer_note = text(&tour.fields.web.note);
    let saw_peer = text(&tour.fields.flutter.saw_peer_note);

    tour.results.record(
        "state_survived_the_teardown",
        has_away && seen == Some(want) && !peer_note.is_empty() && saw_peer == peer_note,
        format!(
            "the new Flutter guest reports {seen:?} line(s) of {want}, including the one written \
             while it did not exist ({has_away}); it rediscovered web.note = {saw_peer:?} after \
             the host wiped its copy. document.lines = {document:?}"
        ),
    );

    let returned = text(&tour.fields.flutter.returned);
    let raised = text(&tour.fields.flutter.raised);
    tour.results.record(
        "the_rebooted_guest_reran_its_commands",
        returned.contains("returned") && raised.contains("raised"),
        format!("returned {returned:?}; raised {raised:?}"),
    );

    tour.step = Step::SampleAfterReboot;
}

/// Samples RSS one last time.
pub fn sample_after_reboot(tour: &mut Tour) {
    tour.samples.push(Sample::take(REBOOTED));
    tour.step = Step::Report;
}

/// Stays alive so `duet dev` has something to hot reload, reporting any change.
///
/// The interesting line is the one printed when `flutter.note` changes: a
/// constant edited in a `.dart` file, patched into a running isolate, written
/// into a store the Rust host never restarted, and delivered to the *other*
/// renderer's watcher.
pub fn linger(tour: &mut Tour) {
    let note = text(&tour.fields.flutter.note);
    if note != tour.last_note {
        tour.last_note.clone_from(&note);
        println!(
            "[showcase] {:>6.2}s  flutter.note is now {note:?}",
            tour.elapsed()
        );
    }
    let seen = text(&tour.fields.web.saw_peer_note);
    if seen != tour.last_peer_note {
        tour.last_peer_note.clone_from(&seen);
        println!(
            "[showcase] {:>6.2}s  the webview guest's watcher has seen {seen:?}",
            tour.elapsed()
        );
    }
    if tour.linger_until.is_some_and(|end| Instant::now() >= end) {
        tour.step = Step::SampleLive;
    }
}

/// Shuts everything down, prints the report, and asks the loop to exit.
pub fn report(tour: &mut Tour) {
    println!();
    println!("=== what each guest published ===");
    for (who, presence) in [
        ("Flutter", &tour.fields.flutter.all),
        ("WebView", &tour.fields.web.all),
    ] {
        match presence.get() {
            Ok(Reading::Present(value)) => println!("  {who}: {value:#?}"),
            other => println!("  {who}: {other:?}"),
        }
    }

    println!();
    println!("=== shutting down ===");

    let handler = tour.flutter.take();
    if let Err(reason) = guests::tear_down_flutter(&mut tour.backend, tour.flutter_id, handler) {
        println!("[showcase] {reason}");
    }
    // The webview surface before its window: the surface holds the `wry`
    // WebView that draws into it.
    tour.webview = None;
    tour.web_window = None;

    let clean = match tour.runtime.take() {
        Some(runtime) => match runtime.shutdown() {
            Ok(()) => (true, "the core thread stopped cleanly".to_string()),
            Err(e) => (false, format!("the core thread refused to stop: {e}")),
        },
        None => (false, "the runtime was already gone".to_string()),
    };
    tour.results
        .record("runtime_shutdown_was_clean", clean.0, clean.1);

    let code = tour.results.print(&tour.samples, tour.timed_out_at);
    // `tao::EventLoop::run` never returns, so there is no `main` left to return
    // an exit code from. Exiting here is what makes this demo checkable from a
    // shell: everything it owns has already been torn down above.
    std::process::exit(tour.exit_code.max(code));
}

/// The difference in kilobytes between two named samples, if both were readable.
fn delta(tour: &Tour, later: &str, earlier: &str) -> Option<i64> {
    let find = |label: &str| tour.samples.iter().find(|s| s.label == label);
    find(later)?.minus(find(earlier)?)
}
