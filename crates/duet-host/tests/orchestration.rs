//! Full orchestration loops, driven only through the public API.

use duet_core::{Instant, Path, Policy, SurfaceState, Value};
use duet_host::{BackendCall, Host, RecordingBackend};
use duet_runtime::{NullSink, Runtime};
use duet_supervisor::{HostEvent, SurfaceAction, WindowId};

fn setup() -> (Host<RecordingBackend>, RecordingBackend, Runtime) {
    let rt = Runtime::spawn(
        Value::map([("editor", Value::map([("zoom", Value::Float(1.0))]))]),
        NullSink,
    );
    let backend = RecordingBackend::new();
    let host = Host::new(rt.handle(), backend.clone());
    (host, backend, rt)
}

/// Runs the host for `ticks` steps of `step` milliseconds, returning every
/// action performed.
///
/// This is the loop the real event loop will run. Earlier phases of this
/// project twice shipped a defect that only appeared *after* the first
/// action — a policy that oscillated forever — because the tests stopped one
/// tick too early. Running the loop and pinning exact totals is what catches
/// that.
fn run(host: &mut Host<RecordingBackend>, ticks: u64, step: u64) -> Vec<SurfaceAction> {
    let mut all = Vec::new();
    for i in 0..ticks {
        all.extend(host.tick(Instant(i * step)));
    }
    all
}

#[test]
fn a_window_opening_and_closing_drives_a_full_renderer_lifecycle() {
    let (mut h, b, rt) = setup();
    let id = h.register(Policy::OnLastWindowClosed { grace_ms: 1_000 });
    let w = WindowId::new(1);

    h.handle_at(
        Instant(0),
        HostEvent::WindowOpened {
            surface: id,
            window: w,
        },
    );
    h.tick(Instant(0));
    assert_eq!(h.state(id), Some(SurfaceState::Live));

    h.handle_at(
        Instant(500),
        HostEvent::WindowClosed {
            surface: id,
            window: w,
        },
    );
    let actions = run(&mut h, 6, 500);

    assert_eq!(
        b.calls(),
        vec![
            BackendCall::StartRenderer(id),
            BackendCall::AttachView(id),
            BackendCall::DetachView(id),
            BackendCall::DestroyRenderer(id),
        ],
        "the full lifecycle must be start, attach, detach, destroy — exactly once each"
    );
    assert!(
        actions.iter().any(|a| a.reclaims_memory()),
        "the loop must reach the action that actually frees memory"
    );
    assert_eq!(h.state(id), Some(SurfaceState::Cold));
    rt.shutdown().expect("shutdown should succeed");
}

#[test]
fn the_loop_settles_and_does_not_oscillate() {
    // Two earlier defects in this project made surfaces cycle forever. Both
    // were invisible to tests that stopped after the first action.
    let (mut h, b, rt) = setup();
    let id = h.register(Policy::OnHidden { grace_ms: 500 });
    let w = WindowId::new(1);

    // Open but never shown: hidden from the start.
    h.handle_at(
        Instant(0),
        HostEvent::WindowOpened {
            surface: id,
            window: w,
        },
    );
    let actions = run(&mut h, 40, 250);

    assert_eq!(
        actions.len(),
        0,
        "a permanently hidden window must never start a renderer, got {actions:?}"
    );
    assert_eq!(b.calls(), vec![], "and must never touch the platform");
    assert_eq!(h.state(id), Some(SurfaceState::Cold));
    rt.shutdown().expect("shutdown should succeed");
}

#[test]
fn two_surfaces_are_orchestrated_independently() {
    let (mut h, b, rt) = setup();
    let flutter = h.register(Policy::OnLastWindowClosed { grace_ms: 500 });
    let webview = h.register(Policy::Never);
    let wf = WindowId::new(1);
    let ww = WindowId::new(2);

    h.handle_at(
        Instant(0),
        HostEvent::WindowOpened {
            surface: flutter,
            window: wf,
        },
    );
    h.handle_at(
        Instant(0),
        HostEvent::WindowOpened {
            surface: webview,
            window: ww,
        },
    );
    h.tick(Instant(0));

    h.handle_at(
        Instant(100),
        HostEvent::WindowClosed {
            surface: flutter,
            window: wf,
        },
    );
    run(&mut h, 6, 200);

    assert!(
        b.calls().contains(&BackendCall::DestroyRenderer(flutter)),
        "the policy-governed surface must be torn down"
    );
    assert!(
        !b.calls().contains(&BackendCall::DestroyRenderer(webview)),
        "a Never-policy surface must survive"
    );
    assert_eq!(h.state(webview), Some(SurfaceState::Live));
    rt.shutdown().expect("shutdown should succeed");
}

#[test]
fn teardown_drops_only_the_torn_down_surfaces_subscriptions() {
    // Dropping the wrong surface's subscriptions would silently stop
    // delivering to a live renderer, and the two surfaces are separate
    // guests.
    let (mut h, _b, rt) = setup();
    let doomed = h.register(Policy::OnLastWindowClosed { grace_ms: 0 });
    let survivor = h.register(Policy::Never);
    let store = h.store_handle().clone();

    let doomed_sub = h.subscriber_for(doomed).expect("registered");
    let survivor_sub = h.subscriber_for(survivor).expect("registered");
    store
        .subscribe(doomed_sub, Path::root())
        .expect("subscribe");
    store
        .subscribe(survivor_sub, Path::root())
        .expect("subscribe");

    let w = WindowId::new(1);
    h.handle_at(
        Instant(0),
        HostEvent::WindowOpened {
            surface: doomed,
            window: w,
        },
    );
    h.tick(Instant(0));
    h.handle_at(
        Instant(10),
        HostEvent::WindowClosed {
            surface: doomed,
            window: w,
        },
    );
    run(&mut h, 4, 10);

    assert_eq!(
        store.drop_subscriber(doomed_sub).expect("query"),
        0,
        "the torn-down surface's subscriptions must already be gone"
    );
    assert_eq!(
        store.drop_subscriber(survivor_sub).expect("query"),
        1,
        "the surviving surface's subscription must be untouched"
    );
    rt.shutdown().expect("shutdown should succeed");
}

#[test]
fn a_backend_failure_leaves_the_surface_failed_and_the_loop_stable() {
    let (mut h, b, rt) = setup();
    let id = h.register(Policy::Never);
    b.fail_next(duet_host::BackendError::Unavailable(
        "no display".to_string(),
    ));

    h.handle_at(
        Instant(0),
        HostEvent::WindowOpened {
            surface: id,
            window: WindowId::new(1),
        },
    );
    let actions = run(&mut h, 20, 100);

    assert!(
        matches!(h.state(id), Some(SurfaceState::Failed(_))),
        "got {:?}",
        h.state(id)
    );
    assert_eq!(
        actions.len(),
        1,
        "a failed surface must be attempted once and then left alone, got {actions:?}"
    );
    rt.shutdown().expect("shutdown should succeed");
}
