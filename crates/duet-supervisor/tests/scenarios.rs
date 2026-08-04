//! End-to-end lifecycle journeys, driven only through the public API.

use duet_core::{Instant, Policy, SurfaceState};
use duet_supervisor::{HostEvent, Supervisor, SurfaceAction};

#[test]
fn a_surface_completes_a_full_open_use_close_teardown_journey() {
    let mut s = Supervisor::new();
    let id = s.register(Policy::OnLastWindowClosed { grace_ms: 5_000 });
    assert_eq!(s.state(id), Some(SurfaceState::Cold));

    // The user opens a window.
    s.handle(&HostEvent::WindowOpened(id));
    s.handle(&HostEvent::WindowShown(id));
    assert_eq!(s.tick(Instant(0)), vec![SurfaceAction::Start(id)]);
    assert_eq!(s.state(id), Some(SurfaceState::Starting));

    // The host brings it up.
    s.handle(&HostEvent::Ready(id));
    assert_eq!(s.state(id), Some(SurfaceState::Live));
    assert_eq!(
        s.tick(Instant(1_000)),
        vec![],
        "a live, windowed surface is left alone"
    );

    // The user closes the last window.
    s.handle(&HostEvent::WindowClosed(id));
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

    s.handle(&HostEvent::WindowOpened(id));
    s.tick(Instant(0));
    s.handle(&HostEvent::Ready(id));

    let mut now = 1_000u64;
    for cycle in 0..5 {
        s.handle(&HostEvent::WindowClosed(id));
        let actions = s.tick(Instant(now));
        assert_eq!(
            actions,
            vec![SurfaceAction::Suspend(id)],
            "cycle {cycle} should begin the grace period"
        );

        now += 1_000; // well inside the 5s grace
        s.handle(&HostEvent::WindowOpened(id));
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
        assert_ne!(
            s.state(id),
            Some(SurfaceState::Cold),
            "cycle {cycle} must never reach Cold"
        );

        s.handle(&HostEvent::Ready(id));
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

    for id in [flutter, webview] {
        s.handle(&HostEvent::WindowOpened(id));
    }
    let actions = s.tick(Instant(0));
    assert_eq!(
        actions,
        vec![SurfaceAction::Start(flutter), SurfaceAction::Start(webview)]
    );
    for id in [flutter, webview] {
        s.handle(&HostEvent::Ready(id));
    }

    // Close both windows. Only the policy-governed surface reacts.
    for id in [flutter, webview] {
        s.handle(&HostEvent::WindowClosed(id));
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

    s.handle(&HostEvent::WindowOpened(id));
    s.tick(Instant(0));
    s.handle(&HostEvent::Failed(id, "renderer crashed".to_string()));
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

    s.handle(&HostEvent::Retry(id));
    assert_eq!(s.state(id), Some(SurfaceState::Starting));
    s.handle(&HostEvent::Ready(id));
    assert_eq!(s.state(id), Some(SurfaceState::Live));
}

#[test]
fn a_torn_down_surface_starts_again_when_a_window_reopens() {
    // Resume-from-Cold: the whole point of keeping state in the host.
    let mut s = Supervisor::new();
    let id = s.register(Policy::OnLastWindowClosed { grace_ms: 0 });

    s.handle(&HostEvent::WindowOpened(id));
    s.tick(Instant(0));
    s.handle(&HostEvent::Ready(id));

    s.handle(&HostEvent::WindowClosed(id));
    assert_eq!(s.tick(Instant(10)), vec![SurfaceAction::Suspend(id)]);
    assert_eq!(s.tick(Instant(11)), vec![SurfaceAction::Teardown(id)]);
    assert_eq!(s.state(id), Some(SurfaceState::Cold));

    s.handle(&HostEvent::WindowOpened(id));
    assert_eq!(s.tick(Instant(12)), vec![SurfaceAction::Start(id)]);
    assert_eq!(s.state(id), Some(SurfaceState::Starting));
}
