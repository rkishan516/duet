# Phase 2b-1 — `duet-supervisor` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Drive `duet-core`'s lifecycle state machine and teardown policy against real surfaces, deciding *when* a renderer starts, suspends and is torn down — the framework's headline behaviour.

**Architecture:** A `Supervisor` owns one `SurfaceState` and `Policy` per surface, consumes window and interaction events, and on each `tick(now)` **returns actions as data** rather than executing them. That keeps it a pure function of (events, now) with no windows, no clock and no store handle — the same "effects as data" decision that made `Store::set` testable, applied one layer up.

**Tech Stack:** Rust 1.92, edition 2024, **zero dependencies beyond `duet-core`**.

**Reference:** `docs/superpowers/specs/2026-08-04-duet-design.md` §5 (lifecycle and policy).
**Evidence:** `docs/superpowers/spikes/2026-08-04-phase0-findings.md` §F3 — Spike A measured that detaching a Flutter view reclaims almost nothing (223 MB → 223 MB); only engine shutdown does (→ 104 MB). So reaching `Cold` is what the framework's headline claim actually depends on, and `Suspending` is a latency optimisation, not a memory state.

---

## Background for the implementer

### What already exists

`duet-core` (merged, zero deps) provides the pieces; **nothing currently drives them.**

```rust
// lifecycle.rs — a pure state machine
struct Instant(pub u64);          // monotonic milliseconds, caller-supplied
enum SurfaceState { Cold, Starting, Live, Suspending { since: Instant }, Failed(String) }
enum LifecycleEvent { Start, Ready, Suspend { at: Instant }, Resume, GraceExpired, Fail(String), Retry }
fn transition(&SurfaceState, &LifecycleEvent) -> Result<SurfaceState, InvalidTransition>

// policy.rs — a pure decision function
enum Policy { OnLastWindowClosed { grace_ms: u64 }, OnHidden { grace_ms: u64 },
              IdleTimeout { after_ms: u64 }, Never }
struct PolicyInput { state: SurfaceState, open_windows: usize, visible_windows: usize,
                     last_interaction: Instant, now: Instant }
enum Decision { NoChange, Suspend, Teardown }
fn evaluate(&Policy, &PolicyInput) -> Decision

impl Decision { fn into_event(self, now: Instant) -> Option<LifecycleEvent> }
```

`Decision::into_event` already exists and maps `Suspend` → `LifecycleEvent::Suspend { at: now }` and `Teardown` → `LifecycleEvent::GraceExpired`. **Use it** — it was added specifically because the `at` value must be the same `now` passed to `evaluate`, and nothing else enforces that.

### What this crate adds

The missing middle. A `Supervisor` that:

1. Tracks, per surface: its `Policy`, its `SurfaceState`, how many of its windows are open and how many visible, and when it was last interacted with.
2. Accepts events from the host — windows opening, closing, showing, hiding; a surface reporting itself ready or failed; user interaction.
3. On `tick(now)`, evaluates each surface's policy, applies the resulting transition, and **returns the actions the host must perform.**

### Why actions-as-data, not callbacks

Identical reasoning to `Store::set` returning `Vec<Notification>`:

- **It stays testable with no platform.** Starting a Flutter engine needs a window server; deciding that one *should* start does not. Keeping the decision separate from the doing is what lets this crate reach high coverage on any machine — the same seam that got `duet-core` to 97%.
- **The host chooses the thread.** Tearing down a surface must happen on the main thread; the supervisor may be ticked from anywhere.
- **Actions are inspectable.** A test asserts on a `Vec<SurfaceAction>` directly, with no mock to write.

### What `Cold` actually means — read this before writing the teardown action

Spike A measured the Flutter side on macOS:

| State | RSS |
|---|---|
| Engine booted, no view | 148 MB |
| View attached | 223 MB |
| **View detached** (`Suspending`) | **223 MB** |
| **Engine shut down** (`Cold`) | **104 MB** |

Detaching reclaims essentially nothing. **Only reaching `Cold` reclaims memory**, so `SurfaceAction::Teardown` is the action that delivers the framework's entire value proposition, and `Suspending` exists purely to avoid paying a ~180 ms engine boot when a user closes and immediately reopens a window.

One consequence for this crate: a surface going `Cold` must also have its store subscriptions dropped, or the store keeps delivering notifications to a renderer that no longer exists. The supervisor does not hold a store handle, so `Teardown` carries that obligation to the host — say so in its docs.

---

## Standing quality bar

Every item below was a real review finding earlier in this project that cost a round trip.

**Documentation**
- Every public item gets a `///` doc comment, **including every enum variant and struct field**. `#![deny(missing_docs)]` enforces it.
- Every `Result`-returning function gets an `# Errors` section.
- **Verify doc claims against the code.** A review found docs promising something a type did not deliver; another found a doc stating drop behaviour backwards.
- If a comment states an invariant, the code must enforce it.

**Tests**
- No tautological assertions — assert shape, not just `is_ok()`.
- **Pin exact counts, not loose bounds.**
- **Property tests pin structure; example tests pin semantics. Include both.** Four algebraic property tests once passed against a mutant only a concrete example caught.
- **Check a fixture can express the distinctions it polices.** This project has been bitten three times: single-character keys made `starts_with` ≡ `==`; a one-element list made "slot *i*" ≡ "slot 0"; and six short-decimal floats hid a bug corrupting 30% of `f64` values.
- Verify each test genuinely fails before the implementation exists.
- **Never assert on wall-clock timing.** All time here is caller-supplied `Instant(u64)`; there is no clock to race.

**Code**
- Functions under 50 lines.
- **Zero dependencies beyond `duet-core`.**
- No `unwrap`/`expect` in non-test code.
- `#![forbid(unsafe_code)]`.

---

## File Structure

| File | Responsibility |
|---|---|
| `crates/duet-supervisor/Cargo.toml` | Manifest; only `duet-core` |
| `crates/duet-supervisor/src/lib.rs` | Crate docs, module decls, re-exports, `Send`/`Sync` assertions |
| `crates/duet-supervisor/src/id.rs` | `SurfaceId` and its allocator |
| `crates/duet-supervisor/src/action.rs` | `SurfaceAction` |
| `crates/duet-supervisor/src/event.rs` | `HostEvent` |
| `crates/duet-supervisor/src/supervisor.rs` | `Supervisor` — registry, event handling, `tick` |
| `crates/duet-supervisor/tests/scenarios.rs` | Integration: full lifecycle journeys |

---

## Task 1: Scaffold, `SurfaceId`, `SurfaceAction`

**Files:**
- Create: `crates/duet-supervisor/Cargo.toml`, `src/lib.rs`, `src/id.rs`, `src/action.rs`
- Modify: root `Cargo.toml`

- [ ] **Step 1: Add to the workspace**

In the root `Cargo.toml`:

```toml
members = ["crates/duet-core", "crates/duet-runtime", "crates/duet-codec", "crates/duet-supervisor"]
```

Leave `exclude = ["spikes"]` untouched.

- [ ] **Step 2: Create the manifest**

Create `crates/duet-supervisor/Cargo.toml`:

```toml
[package]
name = "duet-supervisor"
description = "Decides when Duet surfaces start, suspend and are torn down"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[dependencies]
duet-core = { path = "../duet-core" }
```

`duet-core` is the only dependency. This crate must stay platform-free so it is testable on any machine.

- [ ] **Step 3: Write the failing test**

Create `crates/duet-supervisor/src/id.rs`:

```rust
//! Identifies a surface, and hands out ids that cannot collide.

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
```

- [ ] **Step 4: Run test to verify it fails**

Run: `cargo test -p duet-supervisor`
Expected: FAIL — `cannot find type SurfaceIdAllocator in this scope`.

- [ ] **Step 5: Write the implementation**

Insert above the test module in `crates/duet-supervisor/src/id.rs`:

```rust
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
```

- [ ] **Step 6: Write the failing test for `SurfaceAction`**

Create `crates/duet-supervisor/src/action.rs`:

```rust
//! What the host must do as a result of a supervisor decision.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::SurfaceId;

    #[test]
    fn actions_name_the_surface_they_target() {
        assert_eq!(SurfaceAction::Start(SurfaceId(3)).surface(), SurfaceId(3));
        assert_eq!(SurfaceAction::Resume(SurfaceId(6)).surface(), SurfaceId(6));
        assert_eq!(SurfaceAction::Suspend(SurfaceId(4)).surface(), SurfaceId(4));
        assert_eq!(SurfaceAction::Teardown(SurfaceId(5)).surface(), SurfaceId(5));
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
```

- [ ] **Step 7: Run test to verify it fails**

Run: `cargo test -p duet-supervisor`
Expected: FAIL — `cannot find type SurfaceAction in this scope`.

- [ ] **Step 8: Write the implementation**

Insert above the test module in `crates/duet-supervisor/src/action.rs`:

```rust
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
```

- [ ] **Step 9: Create the crate root**

Create `crates/duet-supervisor/src/lib.rs`:

```rust
//! Decides when Duet surfaces start, suspend and are torn down.
//!
//! [`duet_core`] provides a lifecycle state machine and a teardown policy
//! evaluator, both pure functions. This crate is what drives them against real
//! surfaces: it tracks each surface's state and window counts, consumes host
//! events, and on each [`Supervisor::tick`] returns the [`SurfaceAction`]s the
//! host must perform.
//!
//! # Decisions, not effects
//!
//! The supervisor never acts. Starting a renderer needs a window server;
//! deciding that one *should* start does not. Returning actions as data keeps
//! this crate testable on any machine, lets the host choose which thread
//! performs the work, and makes every decision directly assertable in a test.
//!
//! # What teardown is for
//!
//! Spike A measured the Flutter side: a booted engine with an attached view
//! holds 223 MB, detaching the view still holds 223 MB, and only shutting the
//! engine down drops it to 104 MB. So [`SurfaceAction::Teardown`] is what
//! delivers the framework's headline claim, and the `Suspending` grace period
//! exists purely to avoid paying a ~180 ms engine boot when a user closes and
//! immediately reopens a window.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod action;
pub mod id;

pub use action::SurfaceAction;
pub use id::{SurfaceId, SurfaceIdAllocator};

/// These bounds are load-bearing: a host will tick the supervisor from its
/// event loop while holding it alongside other state. Asserted here so a change
/// that breaks them fails at its own source rather than at an integration point.
const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<SurfaceId>();
    assert_send_sync::<SurfaceAction>();
    assert_send_sync::<SurfaceIdAllocator>();
};
```

The crate docs reference `Supervisor`, which does not exist yet. **If rustdoc warns about the broken intra-doc link, demote it to plain backticks and convert it in the task that adds the type.** Report which you did.

- [ ] **Step 10: Run tests to verify they pass**

Run: `cargo test -p duet-supervisor`
Expected: PASS — 6 passed.

- [ ] **Step 11: Commit**

```bash
git add Cargo.toml Cargo.lock crates/duet-supervisor/
git commit -m "feat(supervisor): scaffold with SurfaceId and SurfaceAction"
```

---

## Task 2: `HostEvent`

**Files:**
- Create: `crates/duet-supervisor/src/event.rs`
- Modify: `crates/duet-supervisor/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/duet-supervisor/src/event.rs`:

```rust
//! What the host tells the supervisor about the world.

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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p duet-supervisor`
Expected: FAIL — `cannot find type HostEvent in this scope`.

- [ ] **Step 3: Write the implementation**

Insert above the test module in `crates/duet-supervisor/src/event.rs`:

```rust
use crate::id::SurfaceId;

/// Something the host observed, reported to the supervisor.
///
/// The supervisor never polls the world; it only knows what it is told. That
/// keeps it a pure function of its event history plus the `now` passed to
/// [`crate::Supervisor::tick`].
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
```

- [ ] **Step 4: Export from `lib.rs`**

Add `pub mod event;` and `pub use event::HostEvent;`, and extend the assertion block with `assert_send_sync::<HostEvent>();`.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p duet-supervisor`
Expected: PASS — 8 passed.

- [ ] **Step 6: Commit**

```bash
git add crates/duet-supervisor/src/
git commit -m "feat(supervisor): add HostEvent"
```

---

## Task 3: `Supervisor` — registration and event tracking

**Files:**
- Create: `crates/duet-supervisor/src/supervisor.rs`
- Modify: `crates/duet-supervisor/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/duet-supervisor/src/supervisor.rs`:

```rust
//! Tracks every surface and decides what should happen to it.

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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p duet-supervisor`
Expected: FAIL — `cannot find type Supervisor in this scope`.

- [ ] **Step 3: Write the implementation**

Insert above the test module in `crates/duet-supervisor/src/supervisor.rs`:

```rust
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
#[derive(Debug, Default)]
pub struct Supervisor {
    surfaces: BTreeMap<SurfaceId, Entry>,
    ids: SurfaceIdAllocator,
    now: Instant,
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
```

- [ ] **Step 4: Export from `lib.rs`**

Add `pub mod supervisor;` and `pub use supervisor::Supervisor;`, and extend the assertion block with `assert_send_sync::<Supervisor>();`. If the crate docs referenced `Supervisor` with plain backticks in Task 1, convert them to intra-doc links now.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p duet-supervisor`
Expected: PASS — 20 passed.

- [ ] **Step 6: Commit**

```bash
git add crates/duet-supervisor/src/
git commit -m "feat(supervisor): track surface state and window counts"
```

---

## Task 4: `tick` — the decision loop

This is the centre of the crate.

**Files:**
- Modify: `crates/duet-supervisor/src/supervisor.rs`

- [ ] **Step 1: Write the failing test**

Add inside `mod tests` in `crates/duet-supervisor/src/supervisor.rs`:

```rust
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
        assert!(matches!(
            s.state(id),
            Some(SurfaceState::Suspending { .. })
        ));

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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p duet-supervisor`
Expected: FAIL — `no method named tick found for struct Supervisor`.

- [ ] **Step 3: Write the implementation**

Add `use duet_core::{Decision, PolicyInput, evaluate};` to the imports and `use crate::action::SurfaceAction;`, then add to `impl Supervisor`:

```rust
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
```

And this free function beside `apply`:

```rust
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
```

Note `SurfaceState::Suspending { .. } if wants_renderer` is matched **before** policy evaluation. That ordering is load-bearing: a reopened window must cancel the teardown, and consulting the policy first would tear down a surface the user just reopened.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p duet-supervisor`
Expected: PASS — 34 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/duet-supervisor/src/
git commit -m "feat(supervisor): decide surface actions on tick"
```

---

## Task 5: Lifecycle journey integration tests

**Files:**
- Create: `crates/duet-supervisor/tests/scenarios.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/duet-supervisor/tests/scenarios.rs`:

```rust
//! End-to-end lifecycle journeys, driven only through the public API.

use duet_core::{Instant, Policy, SurfaceState};
use duet_supervisor::{HostEvent, SurfaceAction, Supervisor};

#[test]
fn a_surface_completes_a_full_open_use_close_teardown_journey() {
    let mut s = Supervisor::new();
    let id = s.register(Policy::OnLastWindowClosed { grace_ms: 5_000 });
    assert_eq!(s.state(id), Some(SurfaceState::Cold));

    // The user opens a window.
    s.handle(&HostEvent::WindowOpened(id));
    s.handle(&HostEvent::WindowShown(id));
    assert_eq!(s.tick(Instant(0)), vec![SurfaceAction::Start(id)]);
    assert_eq!(s.state(id), Some(SurfaceState::Starting));

    // The host brings it up.
    s.handle(&HostEvent::Ready(id));
    assert_eq!(s.state(id), Some(SurfaceState::Live));
    assert_eq!(s.tick(Instant(1_000)), vec![], "a live, windowed surface is left alone");

    // The user closes the last window.
    s.handle(&HostEvent::WindowClosed(id));
    assert_eq!(s.tick(Instant(2_000)), vec![SurfaceAction::Suspend(id)]);

    // Grace elapses.
    assert_eq!(s.tick(Instant(6_999)), vec![]);
    let actions = s.tick(Instant(7_000));
    assert_eq!(actions, vec![SurfaceAction::Teardown(id)]);
    assert!(
        actions[0].reclaims_memory(),
        "teardown is the action that actually frees memory"
    );
    assert_eq!(s.state(id), Some(SurfaceState::Cold));
}

#[test]
fn repeated_close_and_reopen_within_grace_never_reaches_cold() {
    // The anti-thrash property, exercised the way a user actually behaves.
    let mut s = Supervisor::new();
    let id = s.register(Policy::OnLastWindowClosed { grace_ms: 5_000 });

    s.handle(&HostEvent::WindowOpened(id));
    s.tick(Instant(0));
    s.handle(&HostEvent::Ready(id));

    let mut now = 1_000u64;
    for cycle in 0..5 {
        s.handle(&HostEvent::WindowClosed(id));
        let actions = s.tick(Instant(now));
        assert_eq!(
            actions,
            vec![SurfaceAction::Suspend(id)],
            "cycle {cycle} should begin the grace period"
        );

        now += 1_000; // well inside the 5s grace
        s.handle(&HostEvent::WindowOpened(id));
        let actions = s.tick(Instant(now));
        assert_eq!(
            actions,
            vec![SurfaceAction::Resume(id)],
            "cycle {cycle} should reattach, not reboot"
        );
        assert!(
            !actions[0].needs_new_renderer(),
            "cycle {cycle} must not require a fresh engine boot"
        );
        assert_ne!(
            s.state(id),
            Some(SurfaceState::Cold),
            "cycle {cycle} must never reach Cold"
        );

        s.handle(&HostEvent::Ready(id));
        now += 1_000;
    }
}

#[test]
fn two_surfaces_with_different_policies_are_independent() {
    // The real shape: a Flutter surface and a webview surface, each with its
    // own policy, each torn down on its own schedule.
    let mut s = Supervisor::new();
    let flutter = s.register(Policy::OnLastWindowClosed { grace_ms: 1_000 });
    let webview = s.register(Policy::Never);

    for id in [flutter, webview] {
        s.handle(&HostEvent::WindowOpened(id));
    }
    let actions = s.tick(Instant(0));
    assert_eq!(
        actions,
        vec![SurfaceAction::Start(flutter), SurfaceAction::Start(webview)]
    );
    for id in [flutter, webview] {
        s.handle(&HostEvent::Ready(id));
    }

    // Close both windows. Only the policy-governed surface reacts.
    for id in [flutter, webview] {
        s.handle(&HostEvent::WindowClosed(id));
    }
    assert_eq!(s.tick(Instant(100)), vec![SurfaceAction::Suspend(flutter)]);
    assert_eq!(s.tick(Instant(1_100)), vec![SurfaceAction::Teardown(flutter)]);
    assert_eq!(
        s.state(webview),
        Some(SurfaceState::Live),
        "a Never-policy surface survives its windows closing"
    );
}

#[test]
fn a_crashed_surface_stays_failed_until_retried_then_recovers() {
    let mut s = Supervisor::new();
    let id = s.register(Policy::OnLastWindowClosed { grace_ms: 1_000 });

    s.handle(&HostEvent::WindowOpened(id));
    s.tick(Instant(0));
    s.handle(&HostEvent::Failed(id, "renderer crashed".to_string()));
    assert_eq!(
        s.state(id),
        Some(SurfaceState::Failed("renderer crashed".to_string()))
    );

    // Ticking must not thrash a failed surface.
    for t in [100u64, 5_000, 50_000] {
        assert_eq!(s.tick(Instant(t)), vec![], "a failed surface must be left alone at t={t}");
    }

    s.handle(&HostEvent::Retry(id));
    assert_eq!(s.state(id), Some(SurfaceState::Starting));
    s.handle(&HostEvent::Ready(id));
    assert_eq!(s.state(id), Some(SurfaceState::Live));
}

#[test]
fn a_torn_down_surface_starts_again_when_a_window_reopens() {
    // Resume-from-Cold: the whole point of keeping state in the host.
    let mut s = Supervisor::new();
    let id = s.register(Policy::OnLastWindowClosed { grace_ms: 0 });

    s.handle(&HostEvent::WindowOpened(id));
    s.tick(Instant(0));
    s.handle(&HostEvent::Ready(id));

    s.handle(&HostEvent::WindowClosed(id));
    assert_eq!(s.tick(Instant(10)), vec![SurfaceAction::Suspend(id)]);
    assert_eq!(s.tick(Instant(11)), vec![SurfaceAction::Teardown(id)]);
    assert_eq!(s.state(id), Some(SurfaceState::Cold));

    s.handle(&HostEvent::WindowOpened(id));
    assert_eq!(s.tick(Instant(12)), vec![SurfaceAction::Start(id)]);
    assert_eq!(s.state(id), Some(SurfaceState::Starting));
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p duet-supervisor --test scenarios`
Expected: PASS — 5 passed. If a name is missing from the crate root, add the re-export — the integration test may only use the public API, so a missing export is a real finding about the public surface. Report it if so.

- [ ] **Step 3: Commit**

```bash
git add crates/duet-supervisor/tests/
git commit -m "test(supervisor): pin full lifecycle journeys"
```

---

## Task 6: Coverage gate and CI

**Files:**
- Modify: `.github/workflows/duet.yml` only if needed

- [ ] **Step 1: Measure coverage**

Run: `cargo llvm-cov -p duet-supervisor --summary-only`

`cargo-llvm-cov` 0.8.7 is already installed. This forces an instrumented rebuild taking a few minutes — be patient.

Report the real per-file numbers. If any file is below 90% line coverage, read the report and add tests for those branches. **Do not lower the threshold.** If a line is genuinely unreachable, say so explicitly rather than contorting a test.

- [ ] **Step 2: Confirm the workspace gate still passes**

Run: `cargo llvm-cov --workspace --locked --fail-under-lines 90`
Expected: exit 0. Report the workspace total.

- [ ] **Step 3: Verify CI already covers the new crate**

`.github/workflows/duet.yml` runs `--workspace` for fmt, clippy, docs, coverage and the single-threaded pass, so the new crate should be gated automatically. **Read the file and confirm** every step uses `--workspace`. If any step names a specific crate, fix it.

- [ ] **Step 4: Verify every CI step locally**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked
cargo llvm-cov --workspace --locked --fail-under-lines 90
cargo test --workspace --locked -- --test-threads=1
```

- [ ] **Step 5: Commit**

```bash
git add .github/workflows/duet.yml crates/ Cargo.lock
git commit -m "ci: gate duet-supervisor alongside the rest of the workspace"
```

---

## Done criteria

- [ ] `cargo test --workspace` passes — report exact counts per crate
- [ ] `cargo test --workspace -- --test-threads=1` passes with identical counts
- [ ] `cargo llvm-cov --workspace --fail-under-lines 90` exits 0
- [ ] `cargo clippy --workspace --all-targets --locked -- -D warnings` clean
- [ ] `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps` clean
- [ ] `cargo fmt --all -- --check` clean
- [ ] `duet-supervisor`'s only dependency is `duet-core` — `cargo tree -p duet-supervisor`
- [ ] `duet-core`, `duet-runtime` and `duet-codec` are all unchanged — `git diff --stat main -- crates/duet-core crates/duet-runtime crates/duet-codec` is empty
- [ ] No `unwrap`/`expect` in non-test code
- [ ] No clock is read anywhere — `grep` for `std::time`, `SystemTime`, `Instant::now` returns nothing

## What Phase 2b-1 deliberately does not build

- **Window management, the `tao` event loop, the `EventLoopProxy` sink.** All need a window server; they arrive in 2b-2, built against Spike B's proven patterns.
- **The webview surface.** 2b-2.
- **Executing the actions.** The supervisor decides; the host acts. That separation is what makes this crate testable on any machine.
- **Dropping store subscriptions on teardown.** The supervisor holds no store handle by design. `SurfaceAction::Teardown`'s docs carry the obligation to the host.
- **The `Starting`-gap notification buffer.** `duet-runtime`'s crate docs already record that it belongs there, as a `Sink` adapter. It needs a readiness signal from the host, so it lands with 2b-2.
