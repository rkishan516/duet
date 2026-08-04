//! End-to-end lifecycle journeys, driven only through the public API.

use duet_core::{Instant, Policy, SurfaceState};
use duet_supervisor::{HostEvent, Supervisor, SurfaceAction, SurfaceId, WindowId};

#[test]
fn a_surface_completes_a_full_open_use_close_teardown_journey() {
    let mut s = Supervisor::new();
    let id = s.register(Policy::OnLastWindowClosed { grace_ms: 5_000 });
    assert_eq!(s.state(id), Some(SurfaceState::Cold));
    let window = WindowId::new(1);

    // The user opens a window.
    s.handle_at(
        Instant(0),
        HostEvent::WindowOpened {
            surface: id,
            window,
        },
    );
    s.handle_at(
        Instant(0),
        HostEvent::WindowShown {
            surface: id,
            window,
        },
    );
    assert_eq!(s.tick(Instant(0)), vec![SurfaceAction::Start(id)]);
    assert_eq!(s.state(id), Some(SurfaceState::Starting));

    // The host brings it up.
    s.handle_at(Instant(0), HostEvent::Ready(id));
    assert_eq!(s.state(id), Some(SurfaceState::Live));
    assert_eq!(
        s.tick(Instant(1_000)),
        vec![],
        "a live, windowed surface is left alone"
    );

    // The user closes the last window.
    s.handle_at(
        Instant(2_000),
        HostEvent::WindowClosed {
            surface: id,
            window,
        },
    );
    assert_eq!(s.tick(Instant(2_000)), vec![SurfaceAction::Suspend(id)]);

    // Grace elapses.
    assert_eq!(s.tick(Instant(6_999)), vec![]);
    let actions = s.tick(Instant(7_000));
    assert_eq!(actions, vec![SurfaceAction::Teardown(id)]);
    assert!(
        actions[0].reclaims_memory(),
        "teardown is the action that actually frees memory"
    );
    assert_eq!(s.state(id), Some(SurfaceState::Cold));
}

#[test]
fn repeated_close_and_reopen_within_grace_never_reaches_cold() {
    // The anti-thrash property, exercised the way a user actually behaves.
    let mut s = Supervisor::new();
    let id = s.register(Policy::OnLastWindowClosed { grace_ms: 5_000 });
    let window = WindowId::new(1);

    s.handle_at(
        Instant(0),
        HostEvent::WindowOpened {
            surface: id,
            window,
        },
    );
    s.tick(Instant(0));
    s.handle_at(Instant(0), HostEvent::Ready(id));

    let mut now = 1_000u64;
    for cycle in 0..5 {
        s.handle_at(
            Instant(now),
            HostEvent::WindowClosed {
                surface: id,
                window,
            },
        );
        let actions = s.tick(Instant(now));
        assert_eq!(
            actions,
            vec![SurfaceAction::Suspend(id)],
            "cycle {cycle} should begin the grace period"
        );

        now += 1_000; // well inside the 5s grace
        s.handle_at(
            Instant(now),
            HostEvent::WindowOpened {
                surface: id,
                window,
            },
        );
        let actions = s.tick(Instant(now));
        assert_eq!(
            actions,
            vec![SurfaceAction::Resume(id)],
            "cycle {cycle} should reattach, not reboot"
        );
        assert!(
            !actions[0].needs_new_renderer(),
            "cycle {cycle} must not require a fresh engine boot"
        );
        assert_eq!(
            s.state(id),
            Some(SurfaceState::Live),
            "cycle {cycle} must return straight to Live, never pass through Cold"
        );

        now += 1_000;
    }
}

#[test]
fn two_surfaces_with_different_policies_are_independent() {
    // The real shape: a Flutter surface and a webview surface, each with its
    // own policy, each torn down on its own schedule.
    let mut s = Supervisor::new();
    let flutter = s.register(Policy::OnLastWindowClosed { grace_ms: 1_000 });
    let webview = s.register(Policy::Never);
    let window = WindowId::new(1);

    for id in [flutter, webview] {
        s.handle_at(
            Instant(0),
            HostEvent::WindowOpened {
                surface: id,
                window,
            },
        );
    }
    let actions = s.tick(Instant(0));
    assert_eq!(
        actions,
        vec![SurfaceAction::Start(flutter), SurfaceAction::Start(webview)]
    );
    for id in [flutter, webview] {
        s.handle_at(Instant(0), HostEvent::Ready(id));
    }

    // Close both windows. Only the policy-governed surface reacts.
    for id in [flutter, webview] {
        s.handle_at(
            Instant(100),
            HostEvent::WindowClosed {
                surface: id,
                window,
            },
        );
    }
    assert_eq!(s.tick(Instant(100)), vec![SurfaceAction::Suspend(flutter)]);
    assert_eq!(
        s.tick(Instant(1_100)),
        vec![SurfaceAction::Teardown(flutter)]
    );
    assert_eq!(
        s.state(webview),
        Some(SurfaceState::Live),
        "a Never-policy surface survives its windows closing"
    );
}

#[test]
fn a_crashed_surface_stays_failed_until_retried_then_recovers() {
    let mut s = Supervisor::new();
    let id = s.register(Policy::OnLastWindowClosed { grace_ms: 1_000 });
    let window = WindowId::new(1);

    s.handle_at(
        Instant(0),
        HostEvent::WindowOpened {
            surface: id,
            window,
        },
    );
    s.tick(Instant(0));
    s.handle_at(
        Instant(0),
        HostEvent::Failed(id, "renderer crashed".to_string()),
    );
    assert_eq!(
        s.state(id),
        Some(SurfaceState::Failed("renderer crashed".to_string()))
    );

    // Ticking must not thrash a failed surface.
    for t in [100u64, 5_000, 50_000] {
        assert_eq!(
            s.tick(Instant(t)),
            vec![],
            "a failed surface must be left alone at t={t}"
        );
    }

    s.handle_at(Instant(50_000), HostEvent::Retry(id));
    assert_eq!(s.state(id), Some(SurfaceState::Starting));
    s.handle_at(Instant(50_100), HostEvent::Ready(id));
    assert_eq!(s.state(id), Some(SurfaceState::Live));
}

#[test]
fn a_torn_down_surface_starts_again_when_a_window_reopens() {
    // Resume-from-Cold: the whole point of keeping state in the host.
    let mut s = Supervisor::new();
    let id = s.register(Policy::OnLastWindowClosed { grace_ms: 0 });
    let window = WindowId::new(1);

    s.handle_at(
        Instant(0),
        HostEvent::WindowOpened {
            surface: id,
            window,
        },
    );
    s.tick(Instant(0));
    s.handle_at(Instant(0), HostEvent::Ready(id));

    s.handle_at(
        Instant(10),
        HostEvent::WindowClosed {
            surface: id,
            window,
        },
    );
    assert_eq!(s.tick(Instant(10)), vec![SurfaceAction::Suspend(id)]);
    assert_eq!(s.tick(Instant(11)), vec![SurfaceAction::Teardown(id)]);
    assert_eq!(s.state(id), Some(SurfaceState::Cold));

    s.handle_at(
        Instant(12),
        HostEvent::WindowOpened {
            surface: id,
            window,
        },
    );
    assert_eq!(s.tick(Instant(12)), vec![SurfaceAction::Start(id)]);
    assert_eq!(s.state(id), Some(SurfaceState::Starting));
}

/// Runs a realistic host loop through the public API only: tick, and report
/// `Ready` whenever told to bring the surface up. Returns every action
/// emitted, across the whole run, in order.
///
/// A correct implementation settles. An oscillating one emits actions
/// forever, which no test that stops after the first action would catch —
/// exactly how two earlier defects in `duet-supervisor` survived review.
fn run_host_loop(s: &mut Supervisor, id: SurfaceId) -> Vec<SurfaceAction> {
    let mut all = Vec::new();
    for i in 0..40u64 {
        let t = Instant(i * 100);
        for action in s.tick(t) {
            all.push(action);
            if matches!(action, SurfaceAction::Start(_) | SurfaceAction::Resume(_)) {
                s.handle_at(t, HostEvent::Ready(id));
            }
        }
    }
    all
}

#[test]
fn on_hidden_never_starts_a_surface_whose_window_is_only_ever_hidden() {
    // C1's original defect (an oscillating resume) was fixed first; this is
    // the twin defect the same review found next, once probed with a
    // realistic host loop: the Cold arm used "has an open window" as
    // sufficient reason to start, which for OnHidden with a window that is
    // opened but never shown produced a real Start -> Suspend -> Teardown
    // cycle, repeating forever, each one paying a real engine boot for a
    // window nobody could see. A window that stays hidden the whole run
    // must never be started at all.
    let mut s = Supervisor::new();
    let id = s.register(Policy::OnHidden { grace_ms: 1_000 });
    s.handle_at(
        Instant(0),
        HostEvent::WindowOpened {
            surface: id,
            window: WindowId::new(1),
        },
    );

    assert_eq!(run_host_loop(&mut s, id), vec![]);
    assert_eq!(s.state(id), Some(SurfaceState::Cold));
}

#[test]
fn idle_timeout_settles_after_one_cycle_with_no_further_interaction() {
    // The IdleTimeout counterpart. Unlike OnHidden, a freshly registered
    // surface is not yet idle at the instant of registration, so the first
    // tick (which lands at that same instant) still starts it; from there it
    // runs one natural Suspend/Teardown cycle and then -- the property under
    // test -- never restarts, because the same Cold-arm guard now blocks a
    // surface whose interaction has gone stale.
    let mut s = Supervisor::new();
    let id = s.register(Policy::IdleTimeout { after_ms: 1_000 });
    s.handle_at(
        Instant(0),
        HostEvent::WindowOpened {
            surface: id,
            window: WindowId::new(1),
        },
    );

    assert_eq!(
        run_host_loop(&mut s, id),
        vec![
            SurfaceAction::Start(id),
            SurfaceAction::Suspend(id),
            SurfaceAction::Teardown(id),
        ]
    );
    assert_eq!(s.state(id), Some(SurfaceState::Cold));
}

#[test]
fn on_last_window_closed_and_never_start_once_and_stay_live_with_an_open_window() {
    // The policies for which "has an open window" and "policy would not
    // immediately re-suspend" happen to coincide -- OnLastWindowClosed's
    // suspend condition literally is `open_windows == 0`, and Never's
    // `evaluate` is always `NoChange`. Neither should be blocked from
    // starting just because the fix now consults policy on the Cold arm too.
    for policy in [
        Policy::OnLastWindowClosed { grace_ms: 1_000 },
        Policy::Never,
    ] {
        let mut s = Supervisor::new();
        let id = s.register(policy.clone());
        s.handle_at(
            Instant(0),
            HostEvent::WindowOpened {
                surface: id,
                window: WindowId::new(1),
            },
        );

        assert_eq!(
            run_host_loop(&mut s, id),
            vec![SurfaceAction::Start(id)],
            "policy {policy:?}: one boot, then stable at Live"
        );
        assert_eq!(s.state(id), Some(SurfaceState::Live), "policy {policy:?}");
    }
}
