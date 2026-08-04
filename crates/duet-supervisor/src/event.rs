//! What the host tells the supervisor about the world.

use crate::id::{SurfaceId, WindowId};

/// Something the host observed, reported to the supervisor.
///
/// The supervisor never polls the world; it only knows what it is told. That
/// keeps it a pure function of its event history plus the `now` passed to
/// [`crate::Supervisor::handle_at`] and [`crate::Supervisor::tick`].
///
/// The four window variants carry both the surface and the specific window,
/// rather than just the surface. An earlier version counted windows instead
/// of tracking their identity, which could not express which window was
/// being closed or hidden — so `WindowClosed` had to guess whether the
/// window it never named had been visible, and guessed wrong. Naming the
/// window is what makes `PolicyInput::visible_windows <= open_windows` true
/// by construction rather than by convention.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum HostEvent {
    /// A window belonging to a surface was created.
    WindowOpened {
        /// The surface the window belongs to.
        surface: SurfaceId,
        /// The window that was created.
        window: WindowId,
    },
    /// A window belonging to a surface was destroyed.
    WindowClosed {
        /// The surface the window belongs to.
        surface: SurfaceId,
        /// The window that was destroyed.
        window: WindowId,
    },
    /// A window belonging to a surface became visible.
    WindowShown {
        /// The surface the window belongs to.
        surface: SurfaceId,
        /// The window that became visible.
        window: WindowId,
    },
    /// A window belonging to a surface was hidden but not closed.
    ///
    /// Distinct from [`HostEvent::WindowClosed`]: `Policy::OnHidden` acts on
    /// this, `Policy::OnLastWindowClosed` does not.
    WindowHidden {
        /// The surface the window belongs to.
        surface: SurfaceId,
        /// The window that was hidden.
        window: WindowId,
    },
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
            HostEvent::WindowOpened { surface, .. }
            | HostEvent::WindowClosed { surface, .. }
            | HostEvent::WindowShown { surface, .. }
            | HostEvent::WindowHidden { surface, .. }
            | HostEvent::Ready(surface)
            | HostEvent::Failed(surface, _)
            | HostEvent::Interacted(surface)
            | HostEvent::Retry(surface) => *surface,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::SurfaceId;

    #[test]
    fn events_name_the_surface_they_concern() {
        let id = SurfaceId::for_test(2);
        let window = WindowId::new(1);
        assert_eq!(
            HostEvent::WindowOpened {
                surface: id,
                window
            }
            .surface(),
            id
        );
        assert_eq!(
            HostEvent::WindowClosed {
                surface: id,
                window
            }
            .surface(),
            id
        );
        assert_eq!(
            HostEvent::WindowShown {
                surface: id,
                window
            }
            .surface(),
            id
        );
        assert_eq!(
            HostEvent::WindowHidden {
                surface: id,
                window
            }
            .surface(),
            id
        );
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
        let id = SurfaceId::for_test(1);
        let window = WindowId::new(1);
        assert_ne!(
            HostEvent::Interacted(id),
            HostEvent::WindowShown {
                surface: id,
                window
            }
        );
    }

    #[test]
    fn events_naming_different_windows_on_the_same_surface_are_distinct() {
        // The whole point of C2's fix: two window events are not
        // interchangeable just because they name the same surface.
        let id = SurfaceId::for_test(1);
        assert_ne!(
            HostEvent::WindowOpened {
                surface: id,
                window: WindowId::new(1)
            },
            HostEvent::WindowOpened {
                surface: id,
                window: WindowId::new(2)
            }
        );
    }
}
