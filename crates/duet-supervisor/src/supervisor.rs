//! Tracks every surface and decides what should happen to it.

use std::collections::BTreeMap;

use duet_core::{
    Decision, Instant, LifecycleEvent, Policy, PolicyInput, SurfaceState, evaluate, transition,
};

use crate::action::SurfaceAction;
use crate::event::HostEvent;
use crate::id::{SurfaceId, SurfaceIdAllocator, WindowId};

/// Everything the supervisor tracks for one surface.
#[derive(Debug, Clone)]
struct Entry {
    policy: Policy,
    state: SurfaceState,
    // Every window currently open for this surface, keyed by identity, with
    // whether it is currently visible. Tracked by identity rather than
    // counted: a bare counter cannot tell whether the window being closed
    // was the one that was visible, which is exactly the bug this design
    // avoids -- see `HostEvent`'s doc comment.
    windows: BTreeMap<WindowId, bool>,
    last_interaction: Instant,
}

impl Entry {
    /// How many windows are open.
    fn open_windows(&self) -> usize {
        self.windows.len()
    }

    /// How many open windows are visible. Always `<= open_windows` by
    /// construction: a window can only appear here at all if it is a key in
    /// `windows`, i.e. already open.
    fn visible_windows(&self) -> usize {
        self.windows.values().filter(|&&visible| visible).count()
    }
}

/// Tracks every surface and decides what should happen to it.
///
/// Register each surface once, feed it [`HostEvent`]s as the world changes
/// with [`Supervisor::handle_at`], and call [`Supervisor::tick`] to get back
/// the [`crate::SurfaceAction`]s to perform. [`Supervisor::request_suspend`]
/// and [`Supervisor::request_resume`] cover manual overrides that bypass
/// policy entirely.
///
/// Time is caller-supplied throughout: the supervisor never reads a clock, which
/// is what makes every time-dependent behaviour deterministic in tests.
#[derive(Debug, Default)]
pub struct Supervisor {
    surfaces: BTreeMap<SurfaceId, Entry>,
    ids: SurfaceIdAllocator,
}

impl Supervisor {
    /// Creates an empty supervisor.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a surface with its teardown policy, returning its id.
    ///
    /// The surface begins `Cold` with no windows and no recorded
    /// interaction. The initial interaction instant is inert regardless of
    /// its value: policy only ever consults it while `Live`, and every path
    /// that makes a surface `Live` -- `Ready`, the automatic resume inside
    /// [`Supervisor::tick`], and [`Supervisor::request_resume`] -- refreshes
    /// it first.
    pub fn register(&mut self, policy: Policy) -> SurfaceId {
        let id = self.ids.next();
        self.surfaces.insert(
            id,
            Entry {
                policy,
                state: SurfaceState::Cold,
                windows: BTreeMap::new(),
                last_interaction: Instant(0),
            },
        );
        id
    }

    /// Removes a surface. Returns `true` if it was registered.
    ///
    /// Without this, a surface the host has permanently discarded would be
    /// evaluated on every future [`Supervisor::tick`] forever.
    pub fn unregister(&mut self, id: SurfaceId) -> bool {
        self.surfaces.remove(&id).is_some()
    }

    /// The surface's current lifecycle state, or `None` if it is not registered.
    pub fn state(&self, id: SurfaceId) -> Option<SurfaceState> {
        self.surfaces.get(&id).map(|e| e.state.clone())
    }

    /// How many of the surface's windows are open, or `None` if unregistered.
    pub fn open_windows(&self, id: SurfaceId) -> Option<usize> {
        self.surfaces.get(&id).map(Entry::open_windows)
    }

    /// How many of the surface's windows are visible, or `None` if unregistered.
    pub fn visible_windows(&self, id: SurfaceId) -> Option<usize> {
        self.surfaces.get(&id).map(Entry::visible_windows)
    }

    /// When the surface was last interacted with, or `None` if unregistered.
    pub fn last_interaction(&self, id: SurfaceId) -> Option<Instant> {
        self.surfaces.get(&id).map(|e| e.last_interaction)
    }

    /// Applies a host event at the given instant.
    ///
    /// Events naming an unregistered surface are ignored: a host may report a
    /// window closing after its surface has already been dropped, and that is
    /// not an error worth propagating. The same goes for a window event
    /// naming a window this surface never opened, or has already closed.
    ///
    /// `now` is a parameter here rather than tracked internally and set by a
    /// separate call. An earlier version split this into `set_now` plus
    /// `handle`, and forgetting the `set_now` call silently timestamped the
    /// event with whatever `now` a previous `tick` happened to leave behind --
    /// the same `at`-corruption trap `Decision::into_event` exists to close,
    /// reintroduced one layer up. Requiring `now` on every call closes it the
    /// same way.
    pub fn handle_at(&mut self, now: Instant, event: HostEvent) {
        let Some(entry) = self.surfaces.get_mut(&event.surface()) else {
            return;
        };

        match event {
            HostEvent::WindowOpened { window, .. } => {
                entry.windows.insert(window, false);
            }
            HostEvent::WindowClosed { window, .. } => {
                entry.windows.remove(&window);
            }
            HostEvent::WindowShown { window, .. } => {
                // A window that was never opened cannot become visible.
                // Ignoring it here is what keeps `visible_windows <=
                // open_windows` true by construction, not by convention.
                if let Some(visible) = entry.windows.get_mut(&window) {
                    *visible = true;
                }
            }
            HostEvent::WindowHidden { window, .. } => {
                if let Some(visible) = entry.windows.get_mut(&window) {
                    *visible = false;
                }
            }
            HostEvent::Interacted(_) => entry.last_interaction = now,
            HostEvent::Ready(_) => {
                apply(entry, &LifecycleEvent::Ready);
                refresh_if_live(entry, now);
            }
            HostEvent::Failed(_, why) => apply(entry, &LifecycleEvent::Fail(why)),
            HostEvent::Retry(_) => apply(entry, &LifecycleEvent::Retry),
        }
    }

    /// Evaluates every surface's policy and returns the actions the host
    /// must perform.
    ///
    /// Actions come back in `SurfaceId` order, which makes tests and logs
    /// reproducible. Applying them is the host's job -- see
    /// [`SurfaceAction::Teardown`] for an obligation the supervisor cannot
    /// discharge itself.
    pub fn tick(&mut self, now: Instant) -> Vec<SurfaceAction> {
        let mut actions = Vec::new();
        for (id, entry) in &mut self.surfaces {
            if let Some(action) = decide(*id, entry, now) {
                actions.push(action);
            }
        }
        actions
    }

    /// Suspends a surface immediately, bypassing policy.
    ///
    /// Design spec §5.2: manual suspend is always available and always
    /// overrides policy, including [`Policy::Never`]. Returns the action to
    /// perform, or `None` if the surface is unregistered or not currently
    /// `Live` -- suspending only ever applies to a live surface, so there is
    /// nothing to do from any other state.
    pub fn request_suspend(&mut self, id: SurfaceId, now: Instant) -> Option<SurfaceAction> {
        let entry = self.surfaces.get_mut(&id)?;
        if entry.state != SurfaceState::Live {
            return None;
        }
        apply(entry, &LifecycleEvent::Suspend { at: now });
        Some(SurfaceAction::Suspend(id))
    }

    /// Resumes a surface immediately, bypassing policy.
    ///
    /// Design spec §5.2: manual resume is always available and always
    /// overrides policy. Returns the action to perform, or `None` if the
    /// surface is unregistered or not currently `Suspending` -- resuming only
    /// cancels a pending suspension, so there is nothing to do from any
    /// other state.
    pub fn request_resume(&mut self, id: SurfaceId, now: Instant) -> Option<SurfaceAction> {
        let entry = self.surfaces.get_mut(&id)?;
        if !matches!(entry.state, SurfaceState::Suspending { .. }) {
            return None;
        }
        apply(entry, &LifecycleEvent::Resume);
        refresh_if_live(entry, now);
        Some(SurfaceAction::Resume(id))
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

/// Refreshes the idle clock whenever a surface has just become `Live`.
///
/// Applied after `Ready`, after the automatic resume inside [`decide`], and
/// after [`Supervisor::request_resume`]. Without this, a surface that has
/// just finished booting or reattaching is already "idle" under
/// [`Policy::IdleTimeout`] if any time has passed since it was registered (or
/// since it was last interacted with, for a resumed one) -- and can be
/// suspended on its very first tick, having never received any input.
fn refresh_if_live(entry: &mut Entry, now: Instant) {
    if entry.state == SurfaceState::Live {
        entry.last_interaction = now;
    }
}

/// Whether a `Suspending` surface with an open window should resume, rather
/// than let its policy govern the pending teardown.
///
/// Probes the policy as though the surface were already `Live`, and resumes
/// only when the policy would not immediately re-suspend it. Deriving this
/// from [`evaluate`] rather than re-stating each policy's suspend condition
/// is what keeps the two in step: an earlier version used `open_windows > 0`
/// directly, which happens to be the right answer for
/// [`Policy::OnLastWindowClosed`] (whose suspend condition *is*
/// `open_windows == 0`) and is unrelated to the other policies. A
/// [`Policy::OnHidden`] or [`Policy::IdleTimeout`] surface with an open
/// window would resume under that condition and immediately re-qualify for
/// suspension, oscillating between `Live` and `Suspending` forever without
/// ever reaching `Cold`.
fn would_resume(entry: &Entry, now: Instant) -> bool {
    if entry.open_windows() == 0 {
        return false;
    }
    let probe = PolicyInput {
        state: SurfaceState::Live,
        open_windows: entry.open_windows(),
        visible_windows: entry.visible_windows(),
        last_interaction: entry.last_interaction,
        now,
    };
    evaluate(&entry.policy, &probe) != Decision::Suspend
}

/// Decides what should happen to one surface, applying the resulting
/// transition. Returns `None` when nothing should change.
fn decide(id: SurfaceId, entry: &mut Entry, now: Instant) -> Option<SurfaceAction> {
    // A surface with a window but no renderer needs one, whatever the policy
    // says — policy governs teardown, not startup.
    if entry.state == SurfaceState::Cold && entry.open_windows() > 0 {
        apply(entry, &LifecycleEvent::Start);
        return Some(SurfaceAction::Start(id));
    }

    if matches!(entry.state, SurfaceState::Suspending { .. }) && would_resume(entry, now) {
        // Resume cancels the pending teardown without reaching Cold. The
        // renderer was never destroyed, so this is a view reattach rather
        // than an engine boot — hence `Resume`, not `Start`.
        apply(entry, &LifecycleEvent::Resume);
        refresh_if_live(entry, now);
        return Some(SurfaceAction::Resume(id));
    }

    let input = PolicyInput {
        state: entry.state.clone(),
        open_windows: entry.open_windows(),
        visible_windows: entry.visible_windows(),
        last_interaction: entry.last_interaction,
        now,
    };

    // `into_event` exists precisely so the `at` carried by a Suspend is the
    // same `now` that produced the decision; constructing the event by hand
    // here would silently corrupt the grace computation.
    let decision = evaluate(&entry.policy, &input);
    if let Some(event) = decision.into_event(now) {
        apply(entry, &event);
    }

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
    use crate::id::WindowId;
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
        assert_eq!(s.state(SurfaceId::for_test(999)), None);
    }

    #[test]
    fn default_supervisor_starts_empty_like_new() {
        let mut s = Supervisor::default();
        let id = s.register(Policy::Never);
        assert_eq!(s.state(id), Some(SurfaceState::Cold));
        assert_eq!(s.open_windows(id), Some(0));
    }

    #[test]
    fn window_counts_track_open_and_visible_separately() {
        let (mut s, id) = sup();
        let (w1, w2) = (WindowId::new(1), WindowId::new(2));
        s.handle_at(
            Instant(0),
            HostEvent::WindowOpened {
                surface: id,
                window: w1,
            },
        );
        s.handle_at(
            Instant(0),
            HostEvent::WindowOpened {
                surface: id,
                window: w2,
            },
        );
        s.handle_at(
            Instant(0),
            HostEvent::WindowShown {
                surface: id,
                window: w1,
            },
        );
        assert_eq!(s.open_windows(id), Some(2));
        assert_eq!(s.visible_windows(id), Some(1));
    }

    #[test]
    fn hiding_reduces_visible_but_not_open() {
        let (mut s, id) = sup();
        let w = WindowId::new(1);
        s.handle_at(
            Instant(0),
            HostEvent::WindowOpened {
                surface: id,
                window: w,
            },
        );
        s.handle_at(
            Instant(0),
            HostEvent::WindowShown {
                surface: id,
                window: w,
            },
        );
        s.handle_at(
            Instant(0),
            HostEvent::WindowHidden {
                surface: id,
                window: w,
            },
        );
        assert_eq!(s.open_windows(id), Some(1), "hiding must not close");
        assert_eq!(s.visible_windows(id), Some(0));
    }

    #[test]
    fn closing_the_only_open_window_reduces_both_counts() {
        let (mut s, id) = sup();
        let w = WindowId::new(1);
        s.handle_at(
            Instant(0),
            HostEvent::WindowOpened {
                surface: id,
                window: w,
            },
        );
        s.handle_at(
            Instant(0),
            HostEvent::WindowShown {
                surface: id,
                window: w,
            },
        );
        s.handle_at(
            Instant(0),
            HostEvent::WindowClosed {
                surface: id,
                window: w,
            },
        );
        assert_eq!(s.open_windows(id), Some(0));
        assert_eq!(
            s.visible_windows(id),
            Some(0),
            "a closed window cannot still be visible"
        );
    }

    #[test]
    fn closing_one_of_two_open_windows_does_not_suspend() {
        // Distinguishes "no windows open" from "a window closed" -- a
        // single-window fixture cannot express this, since it can't tell
        // "count went to zero" apart from "count went to one".
        let (mut s, id) = sup();
        s.force_state(id, SurfaceState::Live);
        let (w1, w2) = (WindowId::new(1), WindowId::new(2));
        s.handle_at(
            Instant(0),
            HostEvent::WindowOpened {
                surface: id,
                window: w1,
            },
        );
        s.handle_at(
            Instant(0),
            HostEvent::WindowOpened {
                surface: id,
                window: w2,
            },
        );
        assert_eq!(s.open_windows(id), Some(2));

        s.handle_at(
            Instant(100),
            HostEvent::WindowClosed {
                surface: id,
                window: w1,
            },
        );
        assert_eq!(
            s.open_windows(id),
            Some(1),
            "closing one window must leave the other counted"
        );
        assert_eq!(
            s.tick(Instant(200)),
            vec![],
            "one window is still open; the surface must not suspend"
        );
        assert_eq!(s.state(id), Some(SurfaceState::Live));
    }

    #[test]
    fn events_for_unknown_windows_are_ignored() {
        // A host may report a close, or a hide, for a window it never
        // reported opening -- during a startup race, or after a crash. Both
        // must be silently ignored rather than panicking.
        let (mut s, id) = sup();
        let w = WindowId::new(1);
        s.handle_at(
            Instant(0),
            HostEvent::WindowClosed {
                surface: id,
                window: w,
            },
        );
        s.handle_at(
            Instant(0),
            HostEvent::WindowHidden {
                surface: id,
                window: w,
            },
        );
        assert_eq!(s.open_windows(id), Some(0));
        assert_eq!(s.visible_windows(id), Some(0));
    }

    #[test]
    fn showing_or_hiding_an_unopened_window_is_ignored() {
        let (mut s, id) = sup();
        let w = WindowId::new(7);
        s.handle_at(
            Instant(0),
            HostEvent::WindowShown {
                surface: id,
                window: w,
            },
        );
        assert_eq!(
            s.visible_windows(id),
            Some(0),
            "a window that was never opened cannot become visible"
        );
        s.handle_at(
            Instant(0),
            HostEvent::WindowHidden {
                surface: id,
                window: w,
            },
        );
        assert_eq!(s.visible_windows(id), Some(0));
        assert_eq!(
            s.open_windows(id),
            Some(0),
            "neither event should have opened the window as a side effect"
        );
    }

    #[test]
    fn visible_windows_never_exceeds_open_windows_across_event_sequences() {
        // duet_core::PolicyInput::visible_windows documents `<=
        // open_windows` as an invariant. This drives every kind of window
        // event across several windows -- including repeats and events for
        // windows already closed or never opened -- and checks the
        // invariant after every single step, not just at the end.
        let (mut s, id) = sup();
        let windows: Vec<WindowId> = (0..3).map(WindowId::new).collect();
        let events = [
            HostEvent::WindowOpened {
                surface: id,
                window: windows[0],
            },
            HostEvent::WindowShown {
                surface: id,
                window: windows[0],
            },
            HostEvent::WindowOpened {
                surface: id,
                window: windows[1],
            },
            HostEvent::WindowShown {
                surface: id,
                window: windows[1],
            },
            HostEvent::WindowHidden {
                surface: id,
                window: windows[0],
            },
            HostEvent::WindowClosed {
                surface: id,
                window: windows[0],
            },
            HostEvent::WindowClosed {
                surface: id,
                window: windows[0],
            }, // repeat
            HostEvent::WindowHidden {
                surface: id,
                window: windows[0],
            }, // stale
            HostEvent::WindowShown {
                surface: id,
                window: windows[2],
            }, // never opened
            HostEvent::WindowOpened {
                surface: id,
                window: windows[2],
            },
            HostEvent::WindowShown {
                surface: id,
                window: windows[2],
            },
            HostEvent::WindowClosed {
                surface: id,
                window: windows[1],
            },
            HostEvent::WindowClosed {
                surface: id,
                window: windows[2],
            },
        ];

        for (i, event) in events.into_iter().enumerate() {
            s.handle_at(Instant(i as u64), event);
            let open = s.open_windows(id).expect("surface stays registered");
            let visible = s.visible_windows(id).expect("surface stays registered");
            assert!(
                visible <= open,
                "step {i}: visible ({visible}) exceeded open ({open})"
            );
        }
    }

    #[test]
    fn events_for_unknown_surfaces_are_ignored() {
        let (mut s, id) = sup();
        s.handle_at(
            Instant(0),
            HostEvent::WindowOpened {
                surface: SurfaceId::for_test(999),
                window: WindowId::new(1),
            },
        );
        assert_eq!(s.state(SurfaceId::for_test(999)), None);
        assert_eq!(
            s.open_windows(id),
            Some(0),
            "an event for an unknown surface must not disturb a registered one"
        );
    }

    #[test]
    fn interaction_updates_the_idle_clock() {
        let (mut s, id) = sup();
        s.handle_at(Instant(1_000), HostEvent::Interacted(id));
        assert_eq!(s.last_interaction(id), Some(Instant(1_000)));
    }

    #[test]
    fn ready_moves_a_starting_surface_to_live() {
        let (mut s, id) = sup();
        s.force_state(id, SurfaceState::Starting);
        s.handle_at(Instant(0), HostEvent::Ready(id));
        assert_eq!(s.state(id), Some(SurfaceState::Live));
    }

    #[test]
    fn failure_is_recorded_with_its_reason() {
        let (mut s, id) = sup();
        s.handle_at(
            Instant(0),
            HostEvent::Failed(id, "renderer crashed".to_string()),
        );
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
        s.handle_at(Instant(0), HostEvent::Ready(id));
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
        s.handle_at(
            Instant(0),
            HostEvent::WindowOpened {
                surface: id,
                window: WindowId::new(1),
            },
        );
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
        // roughly 180 ms; the grace period exists to avoid paying it.
        let (mut s, id) = sup();
        s.force_state(id, SurfaceState::Live);
        s.tick(Instant(1_000));
        assert!(matches!(s.state(id), Some(SurfaceState::Suspending { .. })));

        s.handle_at(
            Instant(2_000),
            HostEvent::WindowOpened {
                surface: id,
                window: WindowId::new(1),
            },
        );
        let actions = s.tick(Instant(2_000));
        assert_eq!(
            actions,
            vec![SurfaceAction::Resume(id)],
            "a renderer that was only suspended must be reattached, not rebooted"
        );
        assert_eq!(
            s.state(id),
            Some(SurfaceState::Live),
            "the surface must return to Live, never pass through Cold"
        );
    }

    #[test]
    fn a_cold_surface_with_a_window_is_started() {
        let (mut s, id) = sup();
        s.handle_at(
            Instant(0),
            HostEvent::WindowOpened {
                surface: id,
                window: WindowId::new(1),
            },
        );
        assert_eq!(s.tick(Instant(0)), vec![SurfaceAction::Start(id)]);
        assert_eq!(s.state(id), Some(SurfaceState::Starting));
    }

    #[test]
    fn a_starting_surface_is_not_started_again() {
        let (mut s, id) = sup();
        s.handle_at(
            Instant(0),
            HostEvent::WindowOpened {
                surface: id,
                window: WindowId::new(1),
            },
        );
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
        s.handle_at(
            Instant(0),
            HostEvent::WindowOpened {
                surface: id,
                window: WindowId::new(1),
            },
        );
        s.handle_at(Instant(0), HostEvent::Failed(id, "boom".to_string()));
        assert_eq!(s.tick(Instant(1_000)), vec![]);

        s.handle_at(Instant(1_000), HostEvent::Retry(id));
        assert_eq!(s.state(id), Some(SurfaceState::Starting));
    }

    #[test]
    fn never_policy_never_suspends_or_tears_down() {
        let mut s = Supervisor::new();
        let id = s.register(Policy::Never);
        s.force_state(id, SurfaceState::Live);
        for now in [0u64, 1_000, 1_000_000, u64::MAX] {
            assert_eq!(
                s.tick(Instant(now)),
                vec![],
                "Never must stay inert at t={now}"
            );
        }
        assert_eq!(s.state(id), Some(SurfaceState::Live));
    }

    #[test]
    fn on_hidden_policy_reaches_teardown_without_oscillating() {
        let mut s = Supervisor::new();
        let id = s.register(Policy::OnHidden { grace_ms: 1_000 });
        s.force_state(id, SurfaceState::Live);
        let w = WindowId::new(1);
        s.handle_at(
            Instant(0),
            HostEvent::WindowOpened {
                surface: id,
                window: w,
            },
        );
        // Open but never shown: visible == 0, so OnHidden suspends at once.
        assert_eq!(s.tick(Instant(0)), vec![SurfaceAction::Suspend(id)]);

        // The window stays open (merely hidden) through the entire grace
        // period. The bug this test pins: the old `open_windows > 0` resume
        // condition would have resumed the surface on every one of these
        // ticks, since it never checked visibility at all -- the fix must
        // not.
        assert_eq!(s.tick(Instant(500)), vec![], "grace has not elapsed");
        assert_eq!(s.tick(Instant(999)), vec![], "one ms before grace elapses");
        assert_eq!(s.tick(Instant(1_000)), vec![SurfaceAction::Teardown(id)]);
        assert_eq!(s.state(id), Some(SurfaceState::Cold));

        // Close the window now so a later tick cannot auto-restart the
        // surface from Cold (a window still open after teardown legitimately
        // needs a fresh `Start` — see `a_cold_surface_with_a_window_is_started`
        // — which is unrelated to the oscillation property under test here).
        s.handle_at(
            Instant(1_000),
            HostEvent::WindowClosed {
                surface: id,
                window: w,
            },
        );
        assert_eq!(s.tick(Instant(2_000)), vec![]);
        assert_eq!(s.tick(Instant(10_000)), vec![]);
        assert_eq!(s.tick(Instant(100_000)), vec![]);
        assert_eq!(s.state(id), Some(SurfaceState::Cold));
    }

    #[test]
    fn idle_timeout_reaches_teardown_without_oscillating() {
        let mut s = Supervisor::new();
        let id = s.register(Policy::IdleTimeout { after_ms: 1_000 });
        s.force_state(id, SurfaceState::Live);
        let w = WindowId::new(1);
        s.handle_at(
            Instant(0),
            HostEvent::WindowOpened {
                surface: id,
                window: w,
            },
        );
        s.handle_at(Instant(0), HostEvent::Interacted(id));

        assert_eq!(
            s.tick(Instant(999)),
            vec![],
            "one ms before the idle interval elapses"
        );
        assert_eq!(s.tick(Instant(1_000)), vec![SurfaceAction::Suspend(id)]);

        // The window is still open here. The bug this test pins: the old
        // `open_windows > 0` resume condition would have resumed the
        // surface immediately, regardless of idle time -- the fix must not.
        // IdleTimeout carries no separate grace once Suspending (duet-core:
        // the idle interval already served as the grace period), so
        // teardown follows on the very next tick.
        assert_eq!(s.tick(Instant(1_000)), vec![SurfaceAction::Teardown(id)]);
        assert_eq!(s.state(id), Some(SurfaceState::Cold));

        s.handle_at(
            Instant(1_000),
            HostEvent::WindowClosed {
                surface: id,
                window: w,
            },
        );
        assert_eq!(s.tick(Instant(2_000)), vec![]);
        assert_eq!(s.tick(Instant(10_000)), vec![]);
        assert_eq!(s.tick(Instant(100_000)), vec![]);
        assert_eq!(s.state(id), Some(SurfaceState::Cold));
    }

    #[test]
    fn interaction_resets_the_idle_clock() {
        let mut s = Supervisor::new();
        let id = s.register(Policy::IdleTimeout { after_ms: 1_000 });
        s.force_state(id, SurfaceState::Live);
        s.handle_at(
            Instant(0),
            HostEvent::WindowOpened {
                surface: id,
                window: WindowId::new(1),
            },
        );
        s.handle_at(Instant(0), HostEvent::Interacted(id));

        assert_eq!(s.tick(Instant(900)), vec![]);
        s.handle_at(Instant(900), HostEvent::Interacted(id));
        assert_eq!(
            s.tick(Instant(1_500)),
            vec![],
            "the later interaction must push the deadline out"
        );
        assert_eq!(s.tick(Instant(1_900)), vec![SurfaceAction::Suspend(id)]);
    }

    #[test]
    fn a_freshly_started_surface_is_not_immediately_idle() {
        // Mutation testing found that stamping `last_interaction` at
        // registration time and never refreshing it survived the suite: no
        // test registered at a non-zero `now`. The real gap was worse than
        // the mutant -- `last_interaction` was never refreshed when a
        // surface actually became Live, so a surface that had just finished
        // booting could already be "idle" if any time had passed since
        // registration, and get suspended having never received input.
        let mut s = Supervisor::new();
        let id = s.register(Policy::IdleTimeout { after_ms: 1_000 });
        // Register long after zero, then bring the surface up.
        let w = WindowId::new(1);
        s.handle_at(
            Instant(10_000),
            HostEvent::WindowOpened {
                surface: id,
                window: w,
            },
        );
        assert_eq!(s.tick(Instant(10_000)), vec![SurfaceAction::Start(id)]);
        s.handle_at(Instant(10_100), HostEvent::Ready(id));
        assert_eq!(
            s.tick(Instant(10_500)),
            vec![],
            "a surface that just became Live must not count as idle"
        );
    }

    #[test]
    fn manual_suspend_and_resume_override_every_policy_including_never() {
        for policy in [
            Policy::Never,
            Policy::OnLastWindowClosed { grace_ms: 5_000 },
            Policy::OnHidden { grace_ms: 5_000 },
            Policy::IdleTimeout { after_ms: 5_000 },
        ] {
            let mut s = Supervisor::new();
            let id = s.register(policy.clone());
            s.force_state(id, SurfaceState::Live);

            assert_eq!(
                s.request_suspend(id, Instant(0)),
                Some(SurfaceAction::Suspend(id)),
                "policy {policy:?} must not block a manual suspend"
            );
            assert_eq!(
                s.state(id),
                Some(SurfaceState::Suspending { since: Instant(0) })
            );

            assert_eq!(
                s.request_resume(id, Instant(1)),
                Some(SurfaceAction::Resume(id)),
                "policy {policy:?} must not block a manual resume"
            );
            assert_eq!(s.state(id), Some(SurfaceState::Live));
        }
    }

    #[test]
    fn manual_suspend_is_a_no_op_outside_live() {
        let (mut s, id) = sup();
        assert_eq!(s.state(id), Some(SurfaceState::Cold));
        assert_eq!(s.request_suspend(id, Instant(0)), None);
        assert_eq!(s.state(id), Some(SurfaceState::Cold));
    }

    #[test]
    fn manual_resume_is_a_no_op_outside_suspending() {
        let (mut s, id) = sup();
        s.force_state(id, SurfaceState::Live);
        assert_eq!(s.request_resume(id, Instant(0)), None);
        assert_eq!(s.state(id), Some(SurfaceState::Live));
    }

    #[test]
    fn manual_suspend_and_resume_on_an_unregistered_surface_are_no_ops() {
        let mut s = Supervisor::new();
        let id = SurfaceId::for_test(999);
        assert_eq!(s.request_suspend(id, Instant(0)), None);
        assert_eq!(s.request_resume(id, Instant(0)), None);
    }

    #[test]
    fn unregister_removes_a_surface() {
        let (mut s, id) = sup();
        assert!(s.unregister(id));
        assert_eq!(s.state(id), None);
        // Removing it again is a no-op, reported honestly.
        assert!(!s.unregister(id));
    }

    #[test]
    fn an_unregistered_surface_is_never_ticked_again() {
        let (mut s, id) = sup();
        s.force_state(id, SurfaceState::Live);
        s.unregister(id);
        // If it were still tracked, this tick would emit Suspend.
        assert_eq!(s.tick(Instant(1_000)), vec![]);
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
}
