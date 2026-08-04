//! Identifies a surface, and hands out ids that cannot collide.

use std::sync::atomic::{AtomicU64, Ordering};

/// Identifies one surface — one renderer, such as the Flutter side or the
/// webview side.
///
/// Always obtain these from a [`SurfaceIdAllocator`] rather than inventing
/// them. Two surfaces sharing an id would have their lifecycles conflated, and
/// since the two surfaces are separate guests, that crosses a trust boundary.
/// The inner value is private for exactly this reason: a caller cannot
/// construct one by hand even by accident. [`SurfaceId::get`] is the only way
/// to see the wrapped number, and only for uses — logging, an opaque map key
/// — that do not require constructing a new id from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SurfaceId(u64);

impl SurfaceId {
    /// The wrapped value, for logging or as an opaque key in host-side maps.
    ///
    /// There is deliberately no inverse: constructing a [`SurfaceId`] from a
    /// raw `u64` would reopen the collision risk [`SurfaceIdAllocator`]
    /// exists to close.
    pub fn get(self) -> u64 {
        self.0
    }
}

#[cfg(test)]
impl SurfaceId {
    /// Test-only: constructs an id directly, for tests that need one known
    /// not to have come from an allocator — typically to stand in for a
    /// surface that was never registered.
    pub(crate) fn for_test(id: u64) -> Self {
        SurfaceId(id)
    }
}

/// Hands out unique [`SurfaceId`]s.
///
/// Safe to share across threads behind an `Arc`.
#[derive(Debug, Default)]
pub struct SurfaceIdAllocator {
    next: AtomicU64,
}

impl SurfaceIdAllocator {
    /// Creates an allocator starting from zero.
    pub fn new() -> Self {
        Self::default()
    }

    /// Allocates an id no other caller of this allocator will be given.
    pub fn next(&self) -> SurfaceId {
        SurfaceId(self.next.fetch_add(1, Ordering::Relaxed))
    }
}

/// Identifies one window belonging to a surface.
///
/// The host supplies these — in Phase 2b-2 they wrap `tao`'s own `WindowId`.
/// Unlike [`SurfaceId`], the supervisor never allocates one: the window
/// already exists on the host side before the supervisor hears about it, so
/// the host is the only party that can name it.
///
/// [`crate::Supervisor`] tracks windows by identity rather than by count,
/// because a bare counter cannot tell whether the window being closed was the
/// one that was visible — see the [`crate::event::HostEvent`] window variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WindowId(u64);

impl WindowId {
    /// Wraps a host-supplied window identifier.
    pub fn new(id: u64) -> Self {
        WindowId(id)
    }

    /// The wrapped value.
    pub fn get(self) -> u64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn successive_ids_differ() {
        let alloc = SurfaceIdAllocator::new();
        let a = alloc.next();
        let b = alloc.next();
        assert_ne!(a, b, "each allocation must be unique");
    }

    #[test]
    fn ids_are_allocated_in_increasing_order() {
        let alloc = SurfaceIdAllocator::new();
        let ids: Vec<SurfaceId> = (0..5).map(|_| alloc.next()).collect();
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(ids, sorted, "ids must increase monotonically");
    }

    #[test]
    fn allocation_works_from_another_thread() {
        use std::sync::Arc;
        let alloc = Arc::new(SurfaceIdAllocator::new());
        let mine = alloc.next();
        let theirs = {
            let alloc = Arc::clone(&alloc);
            std::thread::spawn(move || alloc.next())
                .join()
                .expect("worker should not panic")
        };
        assert_ne!(mine, theirs, "ids must be unique across threads");
    }

    #[test]
    fn surface_id_get_returns_the_wrapped_value() {
        let alloc = SurfaceIdAllocator::new();
        let first = alloc.next();
        let second = alloc.next();
        assert_eq!(first.get(), 0);
        assert_eq!(second.get(), 1);
    }

    #[test]
    fn window_id_get_returns_the_wrapped_value() {
        assert_eq!(WindowId::new(42).get(), 42);
    }

    #[test]
    fn window_ids_with_different_values_are_distinct() {
        assert_ne!(WindowId::new(1), WindowId::new(2));
        assert_eq!(WindowId::new(1), WindowId::new(1));
    }
}
