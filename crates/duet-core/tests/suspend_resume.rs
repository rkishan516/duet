//! Integration test: state survives a full Live -> Cold -> Live cycle.
//!
//! Everything in this file goes through `duet-core`'s PUBLIC API only —
//! that is the point. If a type or function needed here is not re-exported
//! from the crate root, that is a real finding about the public surface,
//! not a bug in this test.
//!
//! The governing principle under test, in one sentence: **State survives
//! teardown. Events don't.**

use duet_core::{
    Decision, Instant, LifecycleEvent, Path, Policy, PolicyInput, Store, SubscriberId,
    SurfaceState, Value, evaluate, transition,
};

fn p(s: &str) -> Path {
    Path::parse(s).expect("test path should parse")
}

#[test]
fn state_survives_teardown_and_is_restored_on_resume() {
    let mut store = Store::new(Value::map([(
        "editor",
        Value::map([("zoom", Value::Float(1.0))]),
    )]));

    let surface = SubscriberId(1);
    let policy = Policy::OnLastWindowClosed { grace_ms: 5_000 };

    // Surface comes up and subscribes.
    let mut state = transition(&SurfaceState::Cold, &LifecycleEvent::Start).unwrap();
    state = transition(&state, &LifecycleEvent::Ready).unwrap();
    assert_eq!(state, SurfaceState::Live);
    store.subscribe(surface, p("editor.zoom"));

    // It writes something worth keeping.
    store.set(&p("editor.zoom"), Value::Float(3.5)).unwrap();

    // Last window closes.
    let input = PolicyInput {
        state: state.clone(),
        open_windows: 0,
        visible_windows: 0,
        last_interaction: Instant(0),
        now: Instant(1_000),
    };
    let decision = evaluate(&policy, &input);
    assert_eq!(decision, Decision::Suspend);
    state = transition(&state, &decision.into_event(Instant(1_000)).unwrap()).unwrap();

    // Grace elapses, surface is torn down and its subscriptions dropped.
    let input = PolicyInput {
        state: state.clone(),
        now: Instant(6_000),
        ..input
    };
    let decision = evaluate(&policy, &input);
    assert_eq!(decision, Decision::Teardown);
    state = transition(&state, &decision.into_event(Instant(6_000)).unwrap()).unwrap();
    assert_eq!(state, SurfaceState::Cold);
    assert_eq!(store.drop_subscriber(surface), 1);

    // Nothing reaches a cold surface.
    let notes = store.set(&p("editor.zoom"), Value::Float(4.0)).unwrap();
    assert!(notes.is_empty(), "a cold surface must receive nothing");

    // It comes back and re-subscribes.
    state = transition(&state, &LifecycleEvent::Resume).unwrap();
    state = transition(&state, &LifecycleEvent::Ready).unwrap();
    assert_eq!(state, SurfaceState::Live);

    let (_id, snapshot) = store.subscribe(surface, p("editor.zoom"));

    // The whole point: state written before teardown, plus the write that landed
    // while cold, are both present.
    assert_eq!(snapshot, Some(Value::Float(4.0)));
}

#[test]
fn resume_within_grace_returns_to_live_without_going_cold() {
    let state = SurfaceState::Suspending {
        since: Instant(1_000),
    };
    let resumed = transition(&state, &LifecycleEvent::Resume).unwrap();
    assert_eq!(resumed, SurfaceState::Live);
}

// --- Required addition (A): two surfaces, independent lifecycles ---

/// The core promise of the whole framework, checked directly: tearing down
/// one surface (say, the webview reclaiming memory) must not disturb the
/// other (Flutter, still rendering), and the torn-down surface must catch
/// up fully on resume.
#[test]
fn independent_surfaces_survive_teardown_separately() {
    let mut store = Store::new(Value::map([(
        "shared",
        Value::map([("counter", Value::Int(0))]),
    )]));

    let flutter = SubscriberId(1);
    let webview = SubscriberId(2);
    let policy = Policy::OnLastWindowClosed { grace_ms: 5_000 };

    let mut flutter_state = transition(&SurfaceState::Cold, &LifecycleEvent::Start).unwrap();
    flutter_state = transition(&flutter_state, &LifecycleEvent::Ready).unwrap();
    let mut webview_state = transition(&SurfaceState::Cold, &LifecycleEvent::Start).unwrap();
    webview_state = transition(&webview_state, &LifecycleEvent::Ready).unwrap();
    assert_eq!(flutter_state, SurfaceState::Live);
    assert_eq!(webview_state, SurfaceState::Live);

    store.subscribe(flutter, p("shared.counter"));
    let (webview_sub, _) = store.subscribe(webview, p("shared.counter"));

    // Both surfaces are live: an overlapping write reaches both.
    let notes = store.set(&p("shared.counter"), Value::Int(1)).unwrap();
    assert_eq!(notes.len(), 2);
    let mut subs: Vec<u64> = notes.iter().map(|n| n.subscriber.0).collect();
    subs.sort_unstable();
    assert_eq!(subs, vec![flutter.0, webview.0]);

    // Tear down flutter only. Webview's own state is untouched by this.
    let input = PolicyInput {
        state: flutter_state.clone(),
        open_windows: 0,
        visible_windows: 0,
        last_interaction: Instant(0),
        now: Instant(1_000),
    };
    let decision = evaluate(&policy, &input);
    assert_eq!(decision, Decision::Suspend);
    flutter_state = transition(
        &flutter_state,
        &decision.into_event(Instant(1_000)).unwrap(),
    )
    .unwrap();

    let input = PolicyInput {
        state: flutter_state.clone(),
        now: Instant(6_000),
        ..input
    };
    let decision = evaluate(&policy, &input);
    assert_eq!(decision, Decision::Teardown);
    flutter_state = transition(
        &flutter_state,
        &decision.into_event(Instant(6_000)).unwrap(),
    )
    .unwrap();
    assert_eq!(flutter_state, SurfaceState::Cold);
    assert_eq!(store.drop_subscriber(flutter), 1);

    // Webview never left Live.
    assert_eq!(webview_state, SurfaceState::Live);

    // Writes made while flutter is cold reach only the surviving surface.
    let notes = store.set(&p("shared.counter"), Value::Int(2)).unwrap();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].subscriber, webview);
    assert_eq!(notes[0].subscription, webview_sub);

    let notes = store.set(&p("shared.counter"), Value::Int(3)).unwrap();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].subscriber, webview);

    // Flutter resumes and, on resubscribe, sees every change made while it
    // was cold, not just the last one it happened to be looking at.
    flutter_state = transition(&flutter_state, &LifecycleEvent::Resume).unwrap();
    flutter_state = transition(&flutter_state, &LifecycleEvent::Ready).unwrap();
    assert_eq!(flutter_state, SurfaceState::Live);
    let (_id, snapshot) = store.subscribe(flutter, p("shared.counter"));
    assert_eq!(snapshot, Some(Value::Int(3)));
}

// --- Required addition (B): the failure path is observable to the peer ---

/// "If the webview crashes, Flutter can render the error UI" — because
/// surface health is just state, published into the same store as
/// everything else, and read by whichever peer subscribed to it.
#[test]
fn failure_state_is_observable_by_the_peer_surface() {
    let mut store = Store::new(Value::map([(
        "surfaces",
        Value::map([("webview", Value::map([("status", Value::Str("ok".into()))]))]),
    )]));

    let flutter = SubscriberId(1);

    let mut flutter_state = transition(&SurfaceState::Cold, &LifecycleEvent::Start).unwrap();
    flutter_state = transition(&flutter_state, &LifecycleEvent::Ready).unwrap();
    assert_eq!(flutter_state, SurfaceState::Live);

    let mut webview_state = transition(&SurfaceState::Cold, &LifecycleEvent::Start).unwrap();
    webview_state = transition(&webview_state, &LifecycleEvent::Ready).unwrap();
    assert_eq!(webview_state, SurfaceState::Live);

    // Flutter watches the webview's published health.
    let (flutter_sub, _) = store.subscribe(flutter, p("surfaces.webview.status"));

    // The webview crashes.
    webview_state = transition(
        &webview_state,
        &LifecycleEvent::Fail("renderer crashed".into()),
    )
    .unwrap();
    assert_eq!(
        webview_state,
        SurfaceState::Failed("renderer crashed".to_string())
    );

    // The host publishes the failure reason into the store where the
    // healthy peer can see it and render an error UI.
    let notes = store
        .set(
            &p("surfaces.webview.status"),
            Value::Str("renderer crashed".into()),
        )
        .unwrap();

    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].subscriber, flutter);
    assert_eq!(notes[0].subscription, flutter_sub);
    assert_eq!(notes[0].patch.value, Value::Str("renderer crashed".into()));
}

// --- Required addition (C): each policy variant drives a full cycle ---

#[test]
fn on_last_window_closed_drives_a_full_suspend_teardown_resume_cycle() {
    let mut store = Store::new(Value::map([("doc", Value::Str("draft".into()))]));
    let surface = SubscriberId(1);
    let policy = Policy::OnLastWindowClosed { grace_ms: 3_000 };

    let mut state = transition(&SurfaceState::Cold, &LifecycleEvent::Start).unwrap();
    state = transition(&state, &LifecycleEvent::Ready).unwrap();
    store.subscribe(surface, p("doc"));
    store.set(&p("doc"), Value::Str("edited".into())).unwrap();

    let input = PolicyInput {
        state: state.clone(),
        open_windows: 0,
        visible_windows: 0,
        last_interaction: Instant(0),
        now: Instant(100),
    };
    let decision = evaluate(&policy, &input);
    assert_eq!(decision, Decision::Suspend);
    state = transition(&state, &decision.into_event(Instant(100)).unwrap()).unwrap();

    let input = PolicyInput {
        state: state.clone(),
        now: Instant(3_100),
        ..input
    };
    let decision = evaluate(&policy, &input);
    assert_eq!(decision, Decision::Teardown);
    state = transition(&state, &decision.into_event(Instant(3_100)).unwrap()).unwrap();
    assert_eq!(state, SurfaceState::Cold);
    assert_eq!(store.drop_subscriber(surface), 1);

    state = transition(&state, &LifecycleEvent::Resume).unwrap();
    state = transition(&state, &LifecycleEvent::Ready).unwrap();
    assert_eq!(state, SurfaceState::Live);
    let (_id, snapshot) = store.subscribe(surface, p("doc"));
    assert_eq!(snapshot, Some(Value::Str("edited".into())));
}

#[test]
fn on_hidden_drives_a_full_suspend_teardown_resume_cycle() {
    let mut store = Store::new(Value::map([("doc", Value::Str("draft".into()))]));
    let surface = SubscriberId(1);
    let policy = Policy::OnHidden { grace_ms: 2_000 };

    let mut state = transition(&SurfaceState::Cold, &LifecycleEvent::Start).unwrap();
    state = transition(&state, &LifecycleEvent::Ready).unwrap();
    store.subscribe(surface, p("doc"));
    store.set(&p("doc"), Value::Str("edited".into())).unwrap();

    // The window is still open but fully occluded / minimized.
    let input = PolicyInput {
        state: state.clone(),
        open_windows: 1,
        visible_windows: 0,
        last_interaction: Instant(0),
        now: Instant(50),
    };
    let decision = evaluate(&policy, &input);
    assert_eq!(decision, Decision::Suspend);
    state = transition(&state, &decision.into_event(Instant(50)).unwrap()).unwrap();

    let input = PolicyInput {
        state: state.clone(),
        now: Instant(2_050),
        ..input
    };
    let decision = evaluate(&policy, &input);
    assert_eq!(decision, Decision::Teardown);
    state = transition(&state, &decision.into_event(Instant(2_050)).unwrap()).unwrap();
    assert_eq!(state, SurfaceState::Cold);
    assert_eq!(store.drop_subscriber(surface), 1);

    state = transition(&state, &LifecycleEvent::Resume).unwrap();
    state = transition(&state, &LifecycleEvent::Ready).unwrap();
    assert_eq!(state, SurfaceState::Live);
    let (_id, snapshot) = store.subscribe(surface, p("doc"));
    assert_eq!(snapshot, Some(Value::Str("edited".into())));
}

#[test]
fn idle_timeout_drives_a_full_suspend_teardown_resume_cycle() {
    let mut store = Store::new(Value::map([("doc", Value::Str("draft".into()))]));
    let surface = SubscriberId(1);
    let policy = Policy::IdleTimeout { after_ms: 1_000 };

    let mut state = transition(&SurfaceState::Cold, &LifecycleEvent::Start).unwrap();
    state = transition(&state, &LifecycleEvent::Ready).unwrap();
    store.subscribe(surface, p("doc"));
    store.set(&p("doc"), Value::Str("edited".into())).unwrap();

    let input = PolicyInput {
        state: state.clone(),
        open_windows: 1,
        visible_windows: 1,
        last_interaction: Instant(0),
        now: Instant(1_000),
    };
    let decision = evaluate(&policy, &input);
    assert_eq!(decision, Decision::Suspend);
    state = transition(&state, &decision.into_event(Instant(1_000)).unwrap()).unwrap();

    // Idle timeout has no separate grace period: the idle interval already
    // served as the grace, so teardown is immediate once Suspending.
    let input = PolicyInput {
        state: state.clone(),
        now: Instant(1_000),
        ..input
    };
    let decision = evaluate(&policy, &input);
    assert_eq!(decision, Decision::Teardown);
    state = transition(&state, &decision.into_event(Instant(1_000)).unwrap()).unwrap();
    assert_eq!(state, SurfaceState::Cold);
    assert_eq!(store.drop_subscriber(surface), 1);

    state = transition(&state, &LifecycleEvent::Resume).unwrap();
    state = transition(&state, &LifecycleEvent::Ready).unwrap();
    assert_eq!(state, SurfaceState::Live);
    let (_id, snapshot) = store.subscribe(surface, p("doc"));
    assert_eq!(snapshot, Some(Value::Str("edited".into())));
}

#[test]
fn never_policy_never_suspends_or_tears_down_across_varied_inputs() {
    let policy = Policy::Never;

    let live_inputs = [
        PolicyInput {
            state: SurfaceState::Live,
            open_windows: 0,
            visible_windows: 0,
            last_interaction: Instant(0),
            now: Instant(0),
        },
        PolicyInput {
            state: SurfaceState::Live,
            open_windows: 0,
            visible_windows: 0,
            last_interaction: Instant(0),
            now: Instant(u64::MAX),
        },
        PolicyInput {
            state: SurfaceState::Live,
            open_windows: 5,
            visible_windows: 5,
            last_interaction: Instant(1_000),
            now: Instant(1_000),
        },
    ];
    for input in live_inputs {
        assert_eq!(evaluate(&policy, &input), Decision::NoChange);
    }

    let suspending_inputs = [
        PolicyInput {
            state: SurfaceState::Suspending { since: Instant(0) },
            open_windows: 0,
            visible_windows: 0,
            last_interaction: Instant(0),
            now: Instant(0),
        },
        PolicyInput {
            state: SurfaceState::Suspending { since: Instant(0) },
            open_windows: 0,
            visible_windows: 0,
            last_interaction: Instant(0),
            now: Instant(u64::MAX),
        },
    ];
    for input in suspending_inputs {
        assert_eq!(evaluate(&policy, &input), Decision::NoChange);
    }
}

// --- Required addition (D): anti-thrash under repeated close/reopen ---

/// This is exactly what the `Suspending` state exists to prevent: a user
/// rapidly closing and reopening a window must never force a surface all
/// the way to `Cold` (which would mean a full engine boot on the next
/// open), and its subscriptions must never be dropped along the way.
#[test]
fn repeated_close_reopen_within_grace_never_reaches_cold() {
    let mut store = Store::new(Value::map([("doc", Value::Str("draft".into()))]));
    let surface = SubscriberId(1);
    let policy = Policy::OnLastWindowClosed { grace_ms: 5_000 };

    let mut state = transition(&SurfaceState::Cold, &LifecycleEvent::Start).unwrap();
    state = transition(&state, &LifecycleEvent::Ready).unwrap();
    let (sub_id, _) = store.subscribe(surface, p("doc"));

    let mut clock = 0u64;
    for _ in 0..5 {
        // Close: policy says suspend, so the surface enters the grace period.
        clock += 100;
        let close_input = PolicyInput {
            state: state.clone(),
            open_windows: 0,
            visible_windows: 0,
            last_interaction: Instant(clock),
            now: Instant(clock),
        };
        let decision = evaluate(&policy, &close_input);
        assert_eq!(decision, Decision::Suspend);
        state = transition(&state, &decision.into_event(Instant(clock)).unwrap()).unwrap();
        assert!(matches!(state, SurfaceState::Suspending { .. }));

        // Reopen well inside the 5s grace window.
        clock += 200;
        let reopen_input = PolicyInput {
            state: state.clone(),
            open_windows: 1,
            visible_windows: 1,
            last_interaction: Instant(clock),
            now: Instant(clock),
        };
        assert_eq!(
            evaluate(&policy, &reopen_input),
            Decision::NoChange,
            "reopen inside grace must not tear down"
        );
        state = transition(&state, &LifecycleEvent::Resume).unwrap();
        assert_eq!(
            state,
            SurfaceState::Live,
            "must return to Live without ever going Cold"
        );
    }

    // The subscription registered before the churn began is still intact:
    // it was never dropped by any of the five close/reopen cycles above.
    let notes = store.set(&p("doc"), Value::Str("final".into())).unwrap();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].subscription, sub_id);
    assert_eq!(notes[0].subscriber, surface);
}

// --- Required addition (E): events do not survive teardown, state does ---

/// The governing principle, made executable: **State survives teardown.
/// Events don't.**
///
/// A value written to the store while a surface is cold IS visible on
/// resubscribe — it is durable state, held by the host. A notification
/// generated during that same cold period is simply never produced for
/// that surface: its subscription was dropped when it went cold, so there
/// is nothing to replay, queue, or deliver later. Events are transient;
/// only the current state persists.
#[test]
fn state_survives_teardown_but_events_do_not() {
    let mut store = Store::new(Value::map([("doc", Value::Str("v0".into()))]));
    let surface = SubscriberId(1);

    let mut state = transition(&SurfaceState::Cold, &LifecycleEvent::Start).unwrap();
    state = transition(&state, &LifecycleEvent::Ready).unwrap();
    store.subscribe(surface, p("doc"));

    // Surface goes cold (the grace-period path itself is proven elsewhere).
    state = transition(&state, &LifecycleEvent::Suspend { at: Instant(0) }).unwrap();
    state = transition(&state, &LifecycleEvent::GraceExpired).unwrap();
    assert_eq!(state, SurfaceState::Cold);
    assert_eq!(store.drop_subscriber(surface), 1);

    // Two writes land while nobody is subscribed.
    let notes_1 = store.set(&p("doc"), Value::Str("v1".into())).unwrap();
    let notes_2 = store.set(&p("doc"), Value::Str("v2".into())).unwrap();

    // Events don't survive: neither write produced any notification,
    // because the only subscriber was dropped the moment it went cold.
    assert!(
        notes_1.is_empty(),
        "no live subscriber -- no event delivered"
    );
    assert!(
        notes_2.is_empty(),
        "no live subscriber -- no event delivered"
    );

    // State survives: resuming and resubscribing sees the latest value,
    // "v2", even though zero events were ever delivered for either write
    // that produced it, including the intermediate "v1" that nobody ever
    // saw as an event at all.
    state = transition(&state, &LifecycleEvent::Resume).unwrap();
    state = transition(&state, &LifecycleEvent::Ready).unwrap();
    assert_eq!(state, SurfaceState::Live);
    let (_id, snapshot) = store.subscribe(surface, p("doc"));
    assert_eq!(snapshot, Some(Value::Str("v2".into())));
}
