//! Tracks every surface and decides what should happen to it.

use std::collections::BTreeMap;

use duet_core::{
    Decision, Instant, LifecycleEvent, Policy, PolicyInput, SurfaceState, evaluate, transition,
};

use crate::action::SurfaceAction;
use crate::event::HostEvent;
use crate::id::{SurfaceId, SurfaceIdAllocator};

/// Everything the supervisor tracks for one surface.
#[derive(Debug, Clone)]
struct Entry {
    policy: Policy,
    state: SurfaceState,
    open_windows: usize,
    visible_windows: usize,
    last_interaction: Instant,
}

/// Tracks every surface and decides what should happen to it.
///
/// Register each surface once, feed it [`HostEvent`]s as the world changes, and
/// call [`Supervisor::tick`] to get back the [`crate::SurfaceAction`]s to perform.
///
/// Time is caller-supplied throughout: the supervisor never reads a clock, which
/// is what makes every time-dependent behaviour deterministic in tests.
#[derive(Debug)]
pub struct Supervisor {
    surfaces: BTreeMap<SurfaceId, Entry>,
    ids: SurfaceIdAllocator,
    now: Instant,
}

impl Default for Supervisor {
    /// Same as [`Supervisor::new`]. `duet_core::Instant` does not implement
    /// `Default`, so this is written by hand rather than derived.
    fn default() -> Self {
        Self::new()
    }
}

impl Supervisor {
    /// Creates an empty supervisor whose clock starts at zero.
    pub fn new() -> Self {
        Supervisor {
            surfaces: BTreeMap::new(),
            ids: SurfaceIdAllocator::new(),
            now: Instant(0),
        }
    }

    /// Registers a surface with its teardown policy, returning its id.
    ///
    /// The surface begins `Cold` with no windows.
    pub fn register(&mut self, policy: Policy) -> SurfaceId {
        let id = self.ids.next();
        self.surfaces.insert(
            id,
            Entry {
                policy,
                state: SurfaceState::Cold,
                open_windows: 0,
                visible_windows: 0,
                last_interaction: self.now,
            },
        );
        id
    }

    /// The surface's current lifecycle state, or `None` if it is not registered.
    pub fn state(&self, id: SurfaceId) -> Option<SurfaceState> {
        self.surfaces.get(&id).map(|e| e.state.clone())
    }

    /// How many of the surface's windows are open, or `None` if unregistered.
    pub fn open_windows(&self, id: SurfaceId) -> Option<usize> {
        self.surfaces.get(&id).map(|e| e.open_windows)
    }

    /// How many of the surface's windows are visible, or `None` if unregistered.
    pub fn visible_windows(&self, id: SurfaceId) -> Option<usize> {
        self.surfaces.get(&id).map(|e| e.visible_windows)
    }

    /// When the surface was last interacted with, or `None` if unregistered.
    pub fn last_interaction(&self, id: SurfaceId) -> Option<Instant> {
        self.surfaces.get(&id).map(|e| e.last_interaction)
    }

    /// Sets the supervisor's notion of now without evaluating any policy.
    ///
    /// [`Supervisor::tick`] does this for you; this exists so a host can
    /// timestamp incoming events that arrive between ticks.
    pub fn set_now(&mut self, now: Instant) {
        self.now = now;
    }

    /// Applies a host event.
    ///
    /// Events naming an unregistered surface are ignored: a host may report a
    /// window closing after its surface has already been dropped, and that is
    /// not an error worth propagating.
    pub fn handle(&mut self, event: &HostEvent) {
        let now = self.now;
        let Some(entry) = self.surfaces.get_mut(&event.surface()) else {
            return;
        };

        match event {
            HostEvent::WindowOpened(_) => entry.open_windows += 1,
            HostEvent::WindowClosed(_) => {
                // Saturating rather than panicking: a host may report a close
                // it never reported an open for, during a startup race or after
                // a crash, and a buggy host must not take the supervisor down.
                entry.open_windows = entry.open_windows.saturating_sub(1);
                // A closed window cannot still be visible.
                entry.visible_windows = entry.visible_windows.saturating_sub(1);
            }
            HostEvent::WindowShown(_) => entry.visible_windows += 1,
            HostEvent::WindowHidden(_) => {
                entry.visible_windows = entry.visible_windows.saturating_sub(1);
            }
            HostEvent::Interacted(_) => entry.last_interaction = now,
            HostEvent::Ready(_) => apply(entry, &LifecycleEvent::Ready),
            HostEvent::Failed(_, why) => apply(entry, &LifecycleEvent::Fail(why.clone())),
            HostEvent::Retry(_) => apply(entry, &LifecycleEvent::Retry),
        }
    }

    /// Advances the clock, evaluates every surface's policy, and returns the
    /// actions the host must perform.
    ///
    /// Actions come back in `SurfaceId` order, which makes tests and logs
    /// reproducible. Applying them is the host's job — see
    /// [`SurfaceAction::Teardown`] for an obligation the supervisor cannot
    /// discharge itself.
    pub fn tick(&mut self, now: Instant) -> Vec<SurfaceAction> {
        self.now = now;
        let mut actions = Vec::new();

        for (id, entry) in &mut self.surfaces {
            if let Some(action) = decide(*id, entry, now) {
                actions.push(action);
            }
        }
        actions
    }
}

/// Applies a lifecycle event, leaving the state untouched if `duet-core`
/// rejects the transition.
///
/// A rejected transition means the host reported something that does not apply
/// — a `Ready` for a surface that never started, say. Absorbing it is correct:
/// the supervisor's state is the authority, and a stale host event must not
/// corrupt it.
fn apply(entry: &mut Entry, event: &LifecycleEvent) {
    if let Ok(next) = transition(&entry.state, event) {
        entry.state = next;
    }
}

/// Decides what should happen to one surface, applying the resulting
/// transition. Returns `None` when nothing should change.
fn decide(id: SurfaceId, entry: &mut Entry, now: Instant) -> Option<SurfaceAction> {
    // A surface with a window but no renderer needs one, whatever the policy
    // says — policy governs teardown, not startup.
    let wants_renderer = entry.open_windows > 0;
    match entry.state {
        SurfaceState::Cold if wants_renderer => {
            apply(entry, &LifecycleEvent::Start);
            return Some(SurfaceAction::Start(id));
        }
        SurfaceState::Suspending { .. } if wants_renderer => {
            // Resume cancels the pending teardown without reaching Cold. The
            // renderer was never destroyed, so this is a view reattach rather
            // than an engine boot — hence `Resume`, not `Start`. Avoiding that
            // ~180 ms boot is the entire reason the grace period exists.
            apply(entry, &LifecycleEvent::Resume);
            return Some(SurfaceAction::Resume(id));
        }
        _ => {}
    }

    let input = PolicyInput {
        state: entry.state.clone(),
        open_windows: entry.open_windows,
        visible_windows: entry.visible_windows,
        last_interaction: entry.last_interaction,
        now,
    };

    // `into_event` exists precisely so the `at` carried by a Suspend is the
    // same `now` that produced the decision; constructing the event by hand
    // here would silently corrupt the grace computation.
    let decision = evaluate(&entry.policy, &input);
    let event = decision.into_event(now)?;
    apply(entry, &event);

    match decision {
        Decision::NoChange => None,
        Decision::Suspend => Some(SurfaceAction::Suspend(id)),
        Decision::Teardown => Some(SurfaceAction::Teardown(id)),
    }
}

#[cfg(test)]
impl Supervisor {
    /// Forces a state, for tests that need to start from the middle of a
    /// lifecycle without replaying every event to get there.
    fn force_state(&mut self, id: SurfaceId, state: SurfaceState) {
        if let Some(entry) = self.surfaces.get_mut(&id) {
            entry.state = state;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::HostEvent;
    use duet_core::{Instant, Policy, SurfaceState};

    fn sup() -> (Supervisor, SurfaceId) {
        let mut s = Supervisor::new();
        let id = s.register(Policy::OnLastWindowClosed { grace_ms: 5_000 });
        (s, id)
    }

    #[test]
    fn a_registered_surface_starts_cold() {
        let (s, id) = sup();
        assert_eq!(s.state(id), Some(SurfaceState::Cold));
    }

    #[test]
    fn registering_twice_yields_distinct_ids() {
        let mut s = Supervisor::new();
        let a = s.register(Policy::Never);
        let b = s.register(Policy::Never);
        assert_ne!(a, b);
    }

    #[test]
    fn an_unregistered_surface_has_no_state() {
        let (s, _) = sup();
        assert_eq!(s.state(SurfaceId(999)), None);
    }

    #[test]
    fn window_counts_track_open_and_visible_separately() {
        let (mut s, id) = sup();
        s.handle(&HostEvent::WindowOpened(id));
        s.handle(&HostEvent::WindowOpened(id));
        s.handle(&HostEvent::WindowShown(id));
        assert_eq!(s.open_windows(id), Some(2));
        assert_eq!(s.visible_windows(id), Some(1));
    }

    #[test]
    fn hiding_reduces_visible_but_not_open() {
        let (mut s, id) = sup();
        s.handle(&HostEvent::WindowOpened(id));
        s.handle(&HostEvent::WindowShown(id));
        s.handle(&HostEvent::WindowHidden(id));
        assert_eq!(s.open_windows(id), Some(1), "hiding must not close");
        assert_eq!(s.visible_windows(id), Some(0));
    }

    #[test]
    fn closing_a_visible_window_reduces_both() {
        let (mut s, id) = sup();
        s.handle(&HostEvent::WindowOpened(id));
        s.handle(&HostEvent::WindowShown(id));
        s.handle(&HostEvent::WindowClosed(id));
        assert_eq!(s.open_windows(id), Some(0));
        assert_eq!(
            s.visible_windows(id),
            Some(0),
            "a closed window cannot still be visible"
        );
    }

    #[test]
    fn counts_never_go_negative_on_unbalanced_events() {
        // A host may report a close it never reported an open for — during
        // startup races, or after a crash. Saturating rather than panicking
        // keeps a buggy host from taking the supervisor down.
        let (mut s, id) = sup();
        s.handle(&HostEvent::WindowClosed(id));
        s.handle(&HostEvent::WindowHidden(id));
        assert_eq!(s.open_windows(id), Some(0));
        assert_eq!(s.visible_windows(id), Some(0));
    }

    #[test]
    fn events_for_unknown_surfaces_are_ignored() {
        let (mut s, _) = sup();
        s.handle(&HostEvent::WindowOpened(SurfaceId(999)));
        assert_eq!(s.state(SurfaceId(999)), None);
    }

    #[test]
    fn interaction_updates_the_idle_clock() {
        let (mut s, id) = sup();
        s.set_now(Instant(1_000));
        s.handle(&HostEvent::Interacted(id));
        assert_eq!(s.last_interaction(id), Some(Instant(1_000)));
    }

    #[test]
    fn ready_moves_a_starting_surface_to_live() {
        let (mut s, id) = sup();
        s.force_state(id, SurfaceState::Starting);
        s.handle(&HostEvent::Ready(id));
        assert_eq!(s.state(id), Some(SurfaceState::Live));
    }

    #[test]
    fn failure_is_recorded_with_its_reason() {
        let (mut s, id) = sup();
        s.handle(&HostEvent::Failed(id, "renderer crashed".to_string()));
        assert_eq!(
            s.state(id),
            Some(SurfaceState::Failed("renderer crashed".to_string()))
        );
    }

    #[test]
    fn an_invalid_transition_leaves_the_state_unchanged() {
        // `Ready` is meaningless for a Cold surface. duet-core's `transition`
        // rejects it; the supervisor must absorb that rather than panic or
        // corrupt its own state.
        let (mut s, id) = sup();
        s.handle(&HostEvent::Ready(id));
        assert_eq!(
            s.state(id),
            Some(SurfaceState::Cold),
            "a rejected transition must not change state"
        );
    }

    #[test]
    fn a_live_surface_with_no_open_windows_is_suspended() {
        let (mut s, id) = sup();
        s.force_state(id, SurfaceState::Live);
        let actions = s.tick(Instant(1_000));
        assert_eq!(actions, vec![SurfaceAction::Suspend(id)]);
        assert_eq!(
            s.state(id),
            Some(SurfaceState::Suspending {
                since: Instant(1_000)
            }),
            "the returned action and the recorded state must agree"
        );
    }

    #[test]
    fn a_live_surface_with_an_open_window_is_left_alone() {
        let (mut s, id) = sup();
        s.force_state(id, SurfaceState::Live);
        s.handle(&HostEvent::WindowOpened(id));
        assert_eq!(s.tick(Instant(1_000)), vec![]);
        assert_eq!(s.state(id), Some(SurfaceState::Live));
    }

    #[test]
    fn teardown_waits_for_the_grace_period_then_fires_once() {
        let (mut s, id) = sup();
        s.force_state(id, SurfaceState::Live);
        assert_eq!(s.tick(Instant(1_000)), vec![SurfaceAction::Suspend(id)]);

        // One millisecond before the 5s grace expires.
        assert_eq!(s.tick(Instant(5_999)), vec![]);
        // Exactly at expiry — the boundary is inclusive.
        assert_eq!(s.tick(Instant(6_000)), vec![SurfaceAction::Teardown(id)]);
        assert_eq!(s.state(id), Some(SurfaceState::Cold));
        // Already Cold: nothing more to do, however many times we tick.
        assert_eq!(s.tick(Instant(9_999)), vec![]);
    }

    #[test]
    fn reopening_during_grace_cancels_teardown_without_reaching_cold() {
        // The anti-thrash property. Spike A measured a cold engine boot at
        // ~180 ms; the grace period exists to avoid paying it.
        let (mut s, id) = sup();
        s.force_state(id, SurfaceState::Live);
        s.tick(Instant(1_000));
        assert!(matches!(s.state(id), Some(SurfaceState::Suspending { .. })));

        s.handle(&HostEvent::WindowOpened(id));
        let actions = s.tick(Instant(2_000));
        assert_eq!(
            actions,
            vec![SurfaceAction::Resume(id)],
            "a renderer that was only suspended must be reattached, not rebooted"
        );
        assert_ne!(
            s.state(id),
            Some(SurfaceState::Cold),
            "the surface must never reach Cold during the grace window"
        );
    }

    #[test]
    fn a_cold_surface_with_a_window_is_started() {
        let (mut s, id) = sup();
        s.handle(&HostEvent::WindowOpened(id));
        assert_eq!(s.tick(Instant(0)), vec![SurfaceAction::Start(id)]);
        assert_eq!(s.state(id), Some(SurfaceState::Starting));
    }

    #[test]
    fn a_starting_surface_is_not_started_again() {
        let (mut s, id) = sup();
        s.handle(&HostEvent::WindowOpened(id));
        assert_eq!(s.tick(Instant(0)), vec![SurfaceAction::Start(id)]);
        assert_eq!(
            s.tick(Instant(100)),
            vec![],
            "a surface already Starting must not be told to start again"
        );
    }

    #[test]
    fn a_failed_surface_is_left_alone_until_retried() {
        let (mut s, id) = sup();
        s.handle(&HostEvent::WindowOpened(id));
        s.handle(&HostEvent::Failed(id, "boom".to_string()));
        assert_eq!(s.tick(Instant(1_000)), vec![]);

        s.handle(&HostEvent::Retry(id));
        assert_eq!(s.state(id), Some(SurfaceState::Starting));
    }

    #[test]
    fn never_policy_never_suspends_or_tears_down() {
        let mut s = Supervisor::new();
        let id = s.register(Policy::Never);
        s.force_state(id, SurfaceState::Live);
        assert_eq!(s.tick(Instant(u64::MAX)), vec![]);
        assert_eq!(s.state(id), Some(SurfaceState::Live));
    }

    #[test]
    fn on_hidden_policy_suspends_a_visible_count_of_zero() {
        let mut s = Supervisor::new();
        let id = s.register(Policy::OnHidden { grace_ms: 1_000 });
        s.force_state(id, SurfaceState::Live);
        s.handle(&HostEvent::WindowOpened(id));
        // Open but never shown: visible == 0.
        assert_eq!(s.tick(Instant(0)), vec![SurfaceAction::Suspend(id)]);
    }

    #[test]
    fn idle_timeout_suspends_only_after_the_interval() {
        let mut s = Supervisor::new();
        let id = s.register(Policy::IdleTimeout { after_ms: 1_000 });
        s.force_state(id, SurfaceState::Live);
        s.handle(&HostEvent::WindowOpened(id));
        s.set_now(Instant(0));
        s.handle(&HostEvent::Interacted(id));

        assert_eq!(s.tick(Instant(999)), vec![]);
        assert_eq!(s.tick(Instant(1_000)), vec![SurfaceAction::Suspend(id)]);
    }

    #[test]
    fn interaction_resets_the_idle_clock() {
        let mut s = Supervisor::new();
        let id = s.register(Policy::IdleTimeout { after_ms: 1_000 });
        s.force_state(id, SurfaceState::Live);
        s.handle(&HostEvent::WindowOpened(id));
        s.set_now(Instant(0));
        s.handle(&HostEvent::Interacted(id));

        assert_eq!(s.tick(Instant(900)), vec![]);
        s.set_now(Instant(900));
        s.handle(&HostEvent::Interacted(id));
        assert_eq!(
            s.tick(Instant(1_500)),
            vec![],
            "the later interaction must push the deadline out"
        );
        assert_eq!(s.tick(Instant(1_900)), vec![SurfaceAction::Suspend(id)]);
    }

    #[test]
    fn surfaces_are_decided_independently() {
        let mut s = Supervisor::new();
        let a = s.register(Policy::OnLastWindowClosed { grace_ms: 5_000 });
        let b = s.register(Policy::Never);
        s.force_state(a, SurfaceState::Live);
        s.force_state(b, SurfaceState::Live);

        let actions = s.tick(Instant(1_000));
        assert_eq!(
            actions,
            vec![SurfaceAction::Suspend(a)],
            "only the surface whose policy fired should be acted on"
        );
        assert_eq!(s.state(b), Some(SurfaceState::Live));
    }

    #[test]
    fn actions_are_returned_in_surface_id_order() {
        // Deterministic ordering makes tests and logs reproducible. `BTreeMap`
        // gives it for free; this pins that it stays true.
        let mut s = Supervisor::new();
        let ids: Vec<SurfaceId> = (0..4)
            .map(|_| s.register(Policy::OnLastWindowClosed { grace_ms: 0 }))
            .collect();
        for id in &ids {
            s.force_state(*id, SurfaceState::Live);
        }
        let actions = s.tick(Instant(0));
        let targets: Vec<SurfaceId> = actions.iter().map(|a| a.surface()).collect();
        assert_eq!(targets, ids, "actions must come back in id order");
    }

    #[test]
    fn tick_advances_the_clock() {
        let (mut s, id) = sup();
        s.tick(Instant(4_242));
        s.handle(&HostEvent::Interacted(id));
        assert_eq!(
            s.last_interaction(id),
            Some(Instant(4_242)),
            "an event handled after a tick must use that tick's time"
        );
    }
}
