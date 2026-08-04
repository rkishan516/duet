//! Tracks every surface and decides what should happen to it.

use std::collections::BTreeMap;

use duet_core::{Instant, LifecycleEvent, Policy, SurfaceState, transition};

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
}
