//! What the host must do as a result of a supervisor decision.

use crate::id::SurfaceId;

/// Work the host must perform as a result of a supervisor decision.
///
/// The supervisor decides; it never acts. Starting a renderer needs a window
/// server, but deciding that one *should* start does not — keeping the two
/// apart is what lets this crate be tested on any machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SurfaceAction {
    /// Bring the surface up from nothing: create its engine or webview and
    /// attach a view.
    ///
    /// Spike A measured this at roughly 180 ms for the Flutter side. The host
    /// reports completion with `HostEvent::Ready`, or failure with
    /// `HostEvent::Failed`.
    Start(SurfaceId),
    /// Reattach a view to a renderer that is still alive.
    ///
    /// Emitted when a window reopens during the grace period, before the
    /// surface reached [`SurfaceAction::Teardown`]. Distinct from
    /// [`SurfaceAction::Start`] because the work is completely different and
    /// far cheaper — the renderer was never destroyed, so there is no engine to
    /// boot. Avoiding that ~180 ms boot is the entire reason the grace period
    /// exists.
    ///
    /// As with `Start`, the host reports completion with `HostEvent::Ready`.
    Resume(SurfaceId),
    /// Begin the grace period: detach the view but keep the renderer alive.
    ///
    /// Cheap to reverse — this exists so closing and immediately reopening a
    /// window does not pay a full engine boot. It reclaims almost no memory.
    Suspend(SurfaceId),
    /// Destroy the renderer entirely.
    ///
    /// **This is the action that reclaims memory** — Spike A measured 223 MB
    /// before and 104 MB after on the Flutter side, whereas suspending changed
    /// nothing.
    ///
    /// The host must **also drop the surface's store subscriptions**. The
    /// supervisor holds no store handle, so it cannot do this itself, and a
    /// missed drop leaves the store delivering notifications to a renderer that
    /// no longer exists.
    Teardown(SurfaceId),
}

impl SurfaceAction {
    /// The surface this action targets.
    pub fn surface(self) -> SurfaceId {
        match self {
            SurfaceAction::Start(id)
            | SurfaceAction::Resume(id)
            | SurfaceAction::Suspend(id)
            | SurfaceAction::Teardown(id) => id,
        }
    }

    /// Whether performing this action actually frees memory.
    ///
    /// Only [`SurfaceAction::Teardown`] does. Suspending detaches a view, which
    /// Spike A measured as reclaiming essentially nothing.
    pub fn reclaims_memory(self) -> bool {
        matches!(self, SurfaceAction::Teardown(_))
    }

    /// Whether the host must create a renderer from nothing, rather than
    /// reattaching to one that is still alive.
    ///
    /// True only for [`SurfaceAction::Start`]. The distinction matters because
    /// the two cost very different amounts — Spike A measured a cold engine
    /// boot at roughly 180 ms against a near-instant reattach.
    pub fn needs_new_renderer(self) -> bool {
        matches!(self, SurfaceAction::Start(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::SurfaceId;

    #[test]
    fn actions_name_the_surface_they_target() {
        assert_eq!(SurfaceAction::Start(SurfaceId(3)).surface(), SurfaceId(3));
        assert_eq!(SurfaceAction::Resume(SurfaceId(6)).surface(), SurfaceId(6));
        assert_eq!(SurfaceAction::Suspend(SurfaceId(4)).surface(), SurfaceId(4));
        assert_eq!(
            SurfaceAction::Teardown(SurfaceId(5)).surface(),
            SurfaceId(5)
        );
    }

    #[test]
    fn only_teardown_reclaims_memory() {
        // Spike A measured that detaching a view reclaims nothing (223 MB
        // before and after); only shutting the engine down does (104 MB).
        // This predicate exists so a host can log or meter the distinction.
        assert!(!SurfaceAction::Start(SurfaceId(1)).reclaims_memory());
        assert!(!SurfaceAction::Resume(SurfaceId(1)).reclaims_memory());
        assert!(!SurfaceAction::Suspend(SurfaceId(1)).reclaims_memory());
        assert!(SurfaceAction::Teardown(SurfaceId(1)).reclaims_memory());
    }

    #[test]
    fn start_and_resume_are_distinct_because_their_cost_differs() {
        // Spike A: a cold engine boot is ~180 ms; reattaching a view to a
        // renderer that is still alive is near-instant. A host that could not
        // tell them apart would either boot an engine it already has, or try to
        // reattach to one that no longer exists.
        assert_ne!(
            SurfaceAction::Start(SurfaceId(1)),
            SurfaceAction::Resume(SurfaceId(1))
        );
        assert!(SurfaceAction::Start(SurfaceId(1)).needs_new_renderer());
        assert!(!SurfaceAction::Resume(SurfaceId(1)).needs_new_renderer());
    }
}
