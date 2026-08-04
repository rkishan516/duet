//! What the host tells the supervisor about the world.

use crate::id::SurfaceId;

/// Something the host observed, reported to the supervisor.
///
/// The supervisor never polls the world; it only knows what it is told. That
/// keeps it a pure function of its event history plus the `now` passed to
/// `Supervisor::tick`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum HostEvent {
    /// A window belonging to this surface was created.
    WindowOpened(SurfaceId),
    /// A window belonging to this surface was destroyed.
    WindowClosed(SurfaceId),
    /// A window belonging to this surface became visible.
    WindowShown(SurfaceId),
    /// A window belonging to this surface was hidden but not closed.
    ///
    /// Distinct from [`HostEvent::WindowClosed`]: `Policy::OnHidden` acts on
    /// this, `Policy::OnLastWindowClosed` does not.
    WindowHidden(SurfaceId),
    /// The surface finished starting and is rendering.
    Ready(SurfaceId),
    /// The surface failed to start, or its renderer crashed. Carries the reason.
    Failed(SurfaceId, String),
    /// The user interacted with this surface — input, a command, or a store
    /// write originating from it.
    ///
    /// Only `Policy::IdleTimeout` consults this. It is deliberately separate
    /// from window visibility: a window can be visible and idle, or hidden
    /// while its surface is still doing work.
    Interacted(SurfaceId),
    /// Ask a failed surface to start again.
    Retry(SurfaceId),
}

impl HostEvent {
    /// The surface this event concerns.
    pub fn surface(&self) -> SurfaceId {
        match self {
            HostEvent::WindowOpened(id)
            | HostEvent::WindowClosed(id)
            | HostEvent::WindowShown(id)
            | HostEvent::WindowHidden(id)
            | HostEvent::Ready(id)
            | HostEvent::Failed(id, _)
            | HostEvent::Interacted(id)
            | HostEvent::Retry(id) => *id,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::SurfaceId;

    #[test]
    fn events_name_the_surface_they_concern() {
        let id = SurfaceId(2);
        assert_eq!(HostEvent::WindowOpened(id).surface(), id);
        assert_eq!(HostEvent::WindowClosed(id).surface(), id);
        assert_eq!(HostEvent::WindowShown(id).surface(), id);
        assert_eq!(HostEvent::WindowHidden(id).surface(), id);
        assert_eq!(HostEvent::Ready(id).surface(), id);
        assert_eq!(HostEvent::Failed(id, "boom".to_string()).surface(), id);
        assert_eq!(HostEvent::Interacted(id).surface(), id);
        assert_eq!(HostEvent::Retry(id).surface(), id);
    }

    #[test]
    fn interaction_is_distinct_from_window_visibility() {
        // A window can be visible without being interacted with, and
        // interacted with while other windows are hidden. IdleTimeout depends
        // on the difference.
        assert_ne!(
            HostEvent::Interacted(SurfaceId(1)),
            HostEvent::WindowShown(SurfaceId(1))
        );
    }
}
