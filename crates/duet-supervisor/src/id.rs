//! Identifies a surface, and hands out ids that cannot collide.

use std::sync::atomic::{AtomicU64, Ordering};

/// Identifies one surface — one renderer, such as the Flutter side or the
/// webview side.
///
/// Always obtain these from a [`SurfaceIdAllocator`] rather than inventing
/// them. Two surfaces sharing an id would have their lifecycles conflated, and
/// since the two surfaces are separate guests, that crosses a trust boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SurfaceId(pub u64);

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
}
