//! End-to-end lifecycle journeys, driven only through the public API.

use duet_core::{Instant, Policy, SurfaceState};
use duet_supervisor::{HostEvent, Supervisor, SurfaceAction, WindowId};

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

#[test]
fn on_hidden_and_idle_timeout_policies_reach_teardown_without_oscillating() {
    // C1 regression coverage at the integration level, through the public
    // API only: a surface whose window stays open the entire time (merely
    // hidden, for OnHidden; merely idle, for IdleTimeout) must still reach
    // Teardown rather than resuming and re-suspending forever.
    // `open_windows > 0` alone is not the resume condition for either of
    // these policies, only for `OnLastWindowClosed`.
    for policy in [
        Policy::OnHidden { grace_ms: 1_000 },
        Policy::IdleTimeout { after_ms: 1_000 },
    ] {
        let mut s = Supervisor::new();
        let id = s.register(policy.clone());
        let window = WindowId::new(1);

        s.handle_at(
            Instant(0),
            HostEvent::WindowOpened {
                surface: id,
                window,
            },
        );
        assert_eq!(
            s.tick(Instant(0)),
            vec![SurfaceAction::Start(id)],
            "policy {policy:?}"
        );
        s.handle_at(Instant(0), HostEvent::Ready(id));

        // The window is opened but never shown, and no further interaction
        // is ever reported, so both policies suspend at t=1000.
        assert_eq!(
            s.tick(Instant(1_000)),
            vec![SurfaceAction::Suspend(id)],
            "policy {policy:?}"
        );

        // The window stays open throughout. By t=2000 both policies' grace
        // has elapsed (OnHidden's 1000ms grace measured from when
        // Suspending began; IdleTimeout carries none once Suspending) and
        // teardown must have fired, never bounced back to Live.
        assert_eq!(
            s.tick(Instant(2_000)),
            vec![SurfaceAction::Teardown(id)],
            "policy {policy:?}"
        );
        assert_eq!(s.state(id), Some(SurfaceState::Cold), "policy {policy:?}");
    }
}
