# Phase 2b-2 — `duet-host` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Wire the store, the supervisor and the platform together — translating platform events into supervisor input, and supervisor decisions into renderer and window operations.

**Architecture:** A `Host` owns a `Supervisor` and a `StoreHandle`, and drives a `WindowBackend` **trait** rather than `tao` directly. That keeps every orchestration decision testable with no window server; the real `tao`/`wry` backend lands in 2b-3. The `Host` also discharges the one obligation the supervisor cannot: dropping a surface's store subscriptions when it is torn down.

**Tech Stack:** Rust 1.92, edition 2024, dependencies `duet-core`, `duet-runtime`, `duet-supervisor` — **no platform crates**.

**Reference:** `docs/superpowers/specs/2026-08-04-duet-design.md` §3.1 (host-owned windows), §5 (lifecycle).
**Evidence:** Spike B ran this arrangement for 180 s on macOS with 709/709 `EventLoopProxy` events delivered and zero loss, so the mechanism 2b-3 will plug in behind `WindowBackend` is already proven.

---

## Background for the implementer

### What already exists — read these APIs before starting

`duet-supervisor` (merged, zero deps beyond `duet-core`) decides **when**:

```rust
Supervisor::new() -> Supervisor
Supervisor::register(&mut self, Policy) -> SurfaceId
Supervisor::unregister(&mut self, SurfaceId) -> bool
Supervisor::handle_at(&mut self, Instant, HostEvent)
Supervisor::tick(&mut self, Instant) -> Vec<SurfaceAction>
Supervisor::request_suspend(&mut self, SurfaceId, Instant) -> Option<SurfaceAction>
Supervisor::request_resume(&mut self, SurfaceId, Instant) -> Option<SurfaceAction>
Supervisor::state(&self, SurfaceId) -> Option<SurfaceState>

enum HostEvent {
    WindowOpened { surface: SurfaceId, window: WindowId },
    WindowClosed { surface: SurfaceId, window: WindowId },
    WindowShown  { surface: SurfaceId, window: WindowId },
    WindowHidden { surface: SurfaceId, window: WindowId },
    Ready(SurfaceId), Failed(SurfaceId, String), Interacted(SurfaceId), Retry(SurfaceId),
}
enum SurfaceAction { Start(SurfaceId), Resume(SurfaceId), Suspend(SurfaceId), Teardown(SurfaceId) }
```

`SurfaceAction` has two predicates that matter here: `needs_new_renderer()` (true only for `Start`) and `reclaims_memory()` (true only for `Teardown`).

`duet-runtime` (merged) holds the state:

```rust
Runtime::spawn<S: Sink>(Value, S) -> Runtime
Runtime::handle(&self) -> StoreHandle
Runtime::next_subscriber_id(&self) -> SubscriberId
StoreHandle::drop_subscriber(&self, SubscriberId) -> Result<usize, RuntimeError>
```

### What this crate adds

The orchestration that has been missing:

1. **Translate** platform events into `HostEvent`s the supervisor understands.
2. **Execute** the `SurfaceAction`s the supervisor returns, by calling a `WindowBackend`.
3. **Discharge the teardown obligation.** `SurfaceAction::Teardown`'s docs say the host must also drop that surface's store subscriptions — the supervisor holds no store handle and cannot do it. If the host forgets, the store keeps computing and delivering notifications for a renderer that no longer exists.

### Why `WindowBackend` is a trait

The same decision that has now worked four times in this project. `duet-runtime` defined `Sink` rather than depending on `tao`; `duet-supervisor` returns actions rather than performing them. Both reached ~97% coverage on a machine with **no reachable window server** — which Spike A established is exactly this environment.

Creating a window needs a display. Deciding *which* window to create, and reacting to the result, does not. This trait is that line.

### The mapping the host owns

The supervisor knows `SurfaceId` and `WindowId`; the store knows `SubscriberId`. **Nothing links them** — `SurfaceAction::Teardown`'s docs say so explicitly. The host is where that mapping lives, and getting it wrong means either leaking subscriptions or dropping the wrong surface's.

---

## Standing quality bar

Every item below was a real review finding earlier in this project that cost a round trip.

**Documentation**
- Every public item gets a `///` doc comment, **including every enum variant and struct field**. `#![deny(missing_docs)]` enforces it.
- Every `Result`-returning function gets an `# Errors` section.
- **Verify doc claims against the code.** Three separate reviews here found docs stating what the code did not do — one had drop behaviour backwards, one promised a trait that did not exist, one claimed every error variant carried a path when one did not.
- If a comment states an invariant, the code must enforce it. A comment reading *"policy governs teardown, not startup"* was the justification for a defect that broke half the framework's policies.

**Tests — read this twice**
- No tautological assertions; **pin exact counts, not loose bounds.**
- **Close the loop the real system closes.** This project's dominant failure mode, five times over, is a correct test paired with input that cannot fail it: single-character keys made `starts_with` ≡ `==`; a one-element list made "slot *i*" ≡ "slot 0"; six short-decimal floats hid corruption of 30% of `f64` values; and twice, policy tests stopped one tick before an infinite oscillation appeared. **Feed responses back, run past the first action, assert exact totals.**
- Property tests pin structure; example tests pin semantics. Include both.
- Verify each test genuinely fails before the implementation exists.
- **Never assert on wall-clock timing.** Time is caller-supplied `Instant(u64)` throughout.

**Code**
- Functions under 50 lines; no `unwrap`/`expect` in non-test code; `#![forbid(unsafe_code)]`.
- **No platform crates.** If you reach for `tao` or `wry`, stop — that is 2b-3.

---

## File Structure

| File | Responsibility |
|---|---|
| `crates/duet-host/Cargo.toml` | Manifest; `duet-core`, `duet-runtime`, `duet-supervisor` |
| `crates/duet-host/src/lib.rs` | Crate docs, module decls, re-exports, `Send`/`Sync` assertions |
| `crates/duet-host/src/backend.rs` | `WindowBackend` trait, `BackendError`, `RecordingBackend` |
| `crates/duet-host/src/host.rs` | `Host` — surface registry, event translation, action execution |
| `crates/duet-host/tests/orchestration.rs` | Integration: full host loops driven through the public API |

---

## Task 1: Scaffold and `WindowBackend`

**Files:**
- Create: `crates/duet-host/Cargo.toml`, `src/lib.rs`, `src/backend.rs`
- Modify: root `Cargo.toml`

- [ ] **Step 1: Add to the workspace**

In the root `Cargo.toml`, extend `members` with `"crates/duet-host"`. Leave `exclude = ["spikes"]` untouched.

- [ ] **Step 2: Create the manifest**

Create `crates/duet-host/Cargo.toml`:

```toml
[package]
name = "duet-host"
description = "Wires Duet's store, supervisor and platform together"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[dependencies]
duet-core = { path = "../duet-core" }
duet-runtime = { path = "../duet-runtime" }
duet-supervisor = { path = "../duet-supervisor" }
```

**No platform crates.** `tao` and `wry` arrive in 2b-3, behind the trait you are about to write.

- [ ] **Step 3: Write the failing test**

Create `crates/duet-host/src/backend.rs`:

```rust
//! The platform operations a host needs, behind a trait so orchestration is
//! testable without a window server.

#[cfg(test)]
mod tests {
    use super::*;
    use duet_supervisor::SurfaceId;

    #[test]
    fn recording_backend_captures_calls_in_order() {
        let b = RecordingBackend::new();
        b.start_renderer(SurfaceId::from_raw(1)).expect("start should succeed");
        b.attach_view(SurfaceId::from_raw(1)).expect("attach should succeed");
        b.destroy_renderer(SurfaceId::from_raw(1)).expect("destroy should succeed");

        assert_eq!(
            b.calls(),
            vec![
                BackendCall::StartRenderer(SurfaceId::from_raw(1)),
                BackendCall::AttachView(SurfaceId::from_raw(1)),
                BackendCall::DestroyRenderer(SurfaceId::from_raw(1)),
            ]
        );
    }

    #[test]
    fn clones_share_one_recording() {
        // The host takes the backend by value; a test needs a clone to assert on.
        let b = RecordingBackend::new();
        let clone = b.clone();
        clone.start_renderer(SurfaceId::from_raw(2)).expect("start should succeed");
        assert_eq!(b.calls().len(), 1, "a clone and its original must share one log");
    }

    #[test]
    fn a_failing_backend_reports_which_call_failed() {
        let b = RecordingBackend::new();
        b.fail_next(BackendError::Unavailable("no display".to_string()));
        let err = b
            .start_renderer(SurfaceId::from_raw(3))
            .expect_err("the primed failure must surface");
        assert_eq!(err, BackendError::Unavailable("no display".to_string()));
        // The failed call is still recorded — a host that retries must be able
        // to see what was attempted.
        assert_eq!(b.calls().len(), 1);
    }

    #[test]
    fn priming_a_failure_affects_only_the_next_call() {
        let b = RecordingBackend::new();
        b.fail_next(BackendError::Unavailable("transient".to_string()));
        assert!(b.start_renderer(SurfaceId::from_raw(4)).is_err());
        assert!(
            b.start_renderer(SurfaceId::from_raw(4)).is_ok(),
            "the failure must not be sticky"
        );
    }
}
```

- [ ] **Step 4: Run test to verify it fails**

Run: `cargo test -p duet-host`
Expected: FAIL — `cannot find type RecordingBackend in this scope`.

Note the test uses `SurfaceId::from_raw(1)`. `duet-supervisor`'s `SurfaceId` has a private field with only a `get()` accessor, deliberately, so ids come from an allocator. **A test constructor is needed.** Add to `crates/duet-supervisor/src/id.rs`:

```rust
impl SurfaceId {
    /// Builds an id from a raw value.
    ///
    /// Prefer [`SurfaceIdAllocator::next`] — this exists for tests and for a
    /// host reconstructing an id it was previously given. Two surfaces sharing
    /// an id would have their lifecycles conflated across a trust boundary, so
    /// do not mint ids this way in production code.
    pub fn from_raw(id: u64) -> Self {
        SurfaceId(id)
    }
}
```

This is the one change to an already-merged crate in this plan. It is additive, and the doc says plainly when not to use it. **Report it explicitly** so a reviewer sees it was deliberate.

- [ ] **Step 5: Write the implementation**

Insert above the test module in `crates/duet-host/src/backend.rs`:

```rust
use std::sync::{Arc, Mutex};

use duet_supervisor::SurfaceId;

/// Why a platform operation could not be performed.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum BackendError {
    /// The platform could not satisfy the request — no display, no engine
    /// artifacts, or a renderer that failed to boot. Carries the reason.
    Unavailable(String),
}

impl std::fmt::Display for BackendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BackendError::Unavailable(why) => write!(f, "platform operation failed: {why}"),
        }
    }
}

impl std::error::Error for BackendError {}

/// The platform operations a [`crate::Host`] performs.
///
/// This is a trait rather than a direct `tao`/`wry` dependency so that every
/// orchestration decision is testable with no window server present — the same
/// seam that let `duet-runtime` and `duet-supervisor` reach high coverage on any
/// machine. The real backend arrives in Phase 2b-3.
///
/// Implementations run on the main thread: Spike B established that `tao`'s
/// event loop, Flutter's platform thread and the webview all require it.
pub trait WindowBackend {
    /// Creates a renderer for the surface — a Flutter engine or a webview.
    ///
    /// Spike A measured a cold Flutter engine boot at roughly 180 ms on a warm
    /// filesystem cache in a debug build.
    ///
    /// # Errors
    ///
    /// [`BackendError::Unavailable`] if the platform could not create it. The
    /// host reports this to the supervisor as a failure.
    fn start_renderer(&self, surface: SurfaceId) -> Result<(), BackendError>;

    /// Attaches the surface's view to its window, making it render.
    ///
    /// # Errors
    ///
    /// [`BackendError::Unavailable`] if the view could not be attached.
    fn attach_view(&self, surface: SurfaceId) -> Result<(), BackendError>;

    /// Detaches the surface's view, leaving the renderer alive.
    ///
    /// Cheap and cheaply reversed. Spike A measured that this reclaims
    /// essentially no memory — 223 MB before and after — which is why it is
    /// distinct from [`WindowBackend::destroy_renderer`].
    ///
    /// # Errors
    ///
    /// [`BackendError::Unavailable`] if the view could not be detached.
    fn detach_view(&self, surface: SurfaceId) -> Result<(), BackendError>;

    /// Destroys the renderer entirely.
    ///
    /// **This is the operation that reclaims memory** — Spike A measured
    /// 223 MB before and 104 MB after on the Flutter side.
    ///
    /// # Errors
    ///
    /// [`BackendError::Unavailable`] if the renderer could not be destroyed.
    fn destroy_renderer(&self, surface: SurfaceId) -> Result<(), BackendError>;
}

/// One recorded call against a [`RecordingBackend`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendCall {
    /// [`WindowBackend::start_renderer`] was called.
    StartRenderer(SurfaceId),
    /// [`WindowBackend::attach_view`] was called.
    AttachView(SurfaceId),
    /// [`WindowBackend::detach_view`] was called.
    DetachView(SurfaceId),
    /// [`WindowBackend::destroy_renderer`] was called.
    DestroyRenderer(SurfaceId),
}

/// A backend that records every call instead of touching a platform.
///
/// Cloning shares the recording, so a clone can be handed to a [`crate::Host`]
/// while the original is used to assert.
#[derive(Debug, Clone, Default)]
pub struct RecordingBackend {
    inner: Arc<Mutex<Recording>>,
}

#[derive(Debug, Default)]
struct Recording {
    calls: Vec<BackendCall>,
    fail_next: Option<BackendError>,
}

impl RecordingBackend {
    /// Creates an empty recorder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Every call received, in order.
    ///
    /// # Panics
    ///
    /// Panics if a previous holder of the internal lock panicked. This is a
    /// test helper, so surfacing that loudly beats masking it.
    pub fn calls(&self) -> Vec<BackendCall> {
        self.inner.lock().expect("recording lock poisoned").calls.clone()
    }

    /// Makes the next call fail with `error`, then return to succeeding.
    ///
    /// # Panics
    ///
    /// Panics if a previous holder of the internal lock panicked.
    pub fn fail_next(&self, error: BackendError) {
        self.inner.lock().expect("recording lock poisoned").fail_next = Some(error);
    }

    fn record(&self, call: BackendCall) -> Result<(), BackendError> {
        let mut inner = self.inner.lock().map_err(|_| {
            BackendError::Unavailable("recording lock poisoned".to_string())
        })?;
        // Record before failing: a host that retries must be able to see what
        // was attempted.
        inner.calls.push(call);
        match inner.fail_next.take() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

impl WindowBackend for RecordingBackend {
    fn start_renderer(&self, surface: SurfaceId) -> Result<(), BackendError> {
        self.record(BackendCall::StartRenderer(surface))
    }
    fn attach_view(&self, surface: SurfaceId) -> Result<(), BackendError> {
        self.record(BackendCall::AttachView(surface))
    }
    fn detach_view(&self, surface: SurfaceId) -> Result<(), BackendError> {
        self.record(BackendCall::DetachView(surface))
    }
    fn destroy_renderer(&self, surface: SurfaceId) -> Result<(), BackendError> {
        self.record(BackendCall::DestroyRenderer(surface))
    }
}
```

- [ ] **Step 6: Create the crate root**

Create `crates/duet-host/src/lib.rs`:

```rust
//! Wires Duet's store, supervisor and platform together.
//!
//! [`duet_supervisor::Supervisor`] decides *when* a renderer should start,
//! suspend or be torn down; [`duet_runtime`] holds the state. This crate is
//! what connects them to a platform: it translates platform events into
//! supervisor input, executes the supervisor's decisions against a
//! [`WindowBackend`], and discharges the one obligation the supervisor cannot.
//!
//! # The teardown obligation
//!
//! `duet_supervisor::SurfaceAction::Teardown` documents that the host must also
//! drop the surface's store subscriptions — the supervisor holds no store
//! handle. Nothing links a `SurfaceId` to a `SubscriberId`, so that mapping
//! lives here. Forgetting it leaves the store computing and delivering
//! notifications for a renderer that no longer exists.
//!
//! # Why the platform is behind a trait
//!
//! Creating a window needs a display; deciding *which* window to create does
//! not. [`WindowBackend`] is that line, and it is the same seam that let
//! `duet-runtime` and `duet-supervisor` be tested on a machine with no
//! reachable window server. The real `tao`/`wry` backend arrives in Phase 2b-3.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod backend;

pub use backend::{BackendCall, BackendError, RecordingBackend, WindowBackend};

/// These bounds are load-bearing: a backend is moved onto the main thread and
/// its recording shared with assertions. Asserted here so a change that breaks
/// them fails at its own source rather than at an integration point.
const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<BackendError>();
    assert_send_sync::<BackendCall>();
    assert_send_sync::<RecordingBackend>();
};
```

The crate docs reference `Host`, which does not exist yet. **If rustdoc warns about the broken intra-doc link, demote it to plain backticks** and convert it in the task that adds the type. Report which you did.

- [ ] **Step 7: Run tests to verify they pass**

Run: `cargo test -p duet-host`
Expected: PASS — 4 passed.

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml Cargo.lock crates/duet-host/ crates/duet-supervisor/src/id.rs
git commit -m "feat(host): scaffold with the WindowBackend seam"
```

---

## Task 2: `Host` — registration and the subscriber mapping

**Files:**
- Create: `crates/duet-host/src/host.rs`
- Modify: `crates/duet-host/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/duet-host/src/host.rs`:

```rust
//! Orchestrates the supervisor, the store and a platform backend.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{BackendCall, RecordingBackend};
    use duet_core::{Instant, Policy, Value};
    use duet_runtime::{NullSink, Runtime};

    fn host() -> (Host<RecordingBackend>, RecordingBackend, Runtime) {
        let rt = Runtime::spawn(Value::map([("k", Value::Int(0))]), NullSink);
        let backend = RecordingBackend::new();
        let host = Host::new(rt.handle(), backend.clone());
        (host, backend, rt)
    }

    #[test]
    fn a_registered_surface_gets_its_own_subscriber_id() {
        let (mut h, _b, rt) = host();
        let a = h.register(Policy::Never);
        let b = h.register(Policy::Never);
        assert_ne!(
            h.subscriber_for(a),
            h.subscriber_for(b),
            "each surface must own a distinct subscriber, or their notifications cross"
        );
        assert!(h.subscriber_for(a).is_some());
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn an_unregistered_surface_has_no_subscriber() {
        let (h, _b, rt) = host();
        assert_eq!(h.subscriber_for(SurfaceId::from_raw(999)), None);
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn registering_does_not_touch_the_backend() {
        // Registration is bookkeeping. Nothing platform-facing happens until
        // a window opens and the supervisor decides to start the surface.
        let (mut h, b, rt) = host();
        h.register(Policy::Never);
        assert_eq!(b.calls(), vec![], "registration must not create a renderer");
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn unregistering_releases_the_subscriber_mapping() {
        let (mut h, _b, rt) = host();
        let id = h.register(Policy::Never);
        assert!(h.unregister(id));
        assert_eq!(h.subscriber_for(id), None);
        assert!(!h.unregister(id), "a second unregister reports absence");
        rt.shutdown().expect("shutdown should succeed");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p duet-host`
Expected: FAIL — `cannot find type Host in this scope`.

- [ ] **Step 3: Write the implementation**

Insert above the test module in `crates/duet-host/src/host.rs`:

```rust
use std::collections::BTreeMap;

use duet_core::{Instant, Policy, SurfaceState};
use duet_runtime::{StoreHandle, SubscriberId};
use duet_supervisor::{HostEvent, SurfaceAction, SurfaceId, Supervisor};

use crate::backend::{BackendError, WindowBackend};

/// Orchestrates the supervisor, the store and a platform backend.
///
/// Owns the `SurfaceId` → `SubscriberId` mapping, which exists nowhere else:
/// the supervisor has no store handle and the store knows nothing of surfaces.
#[derive(Debug)]
pub struct Host<B: WindowBackend> {
    supervisor: Supervisor,
    store: StoreHandle,
    backend: B,
    subscribers: BTreeMap<SurfaceId, SubscriberId>,
}

impl<B: WindowBackend> Host<B> {
    /// Creates a host over a store handle and a platform backend.
    pub fn new(store: StoreHandle, backend: B) -> Self {
        Host {
            supervisor: Supervisor::new(),
            store,
            backend,
            subscribers: BTreeMap::new(),
        }
    }

    /// Registers a surface with its teardown policy.
    ///
    /// Allocates the surface a `SubscriberId` of its own. Two surfaces sharing
    /// one would have each other's notifications delivered to them, which
    /// crosses the trust boundary between the two guests.
    ///
    /// Registration is bookkeeping only — no renderer is created until a window
    /// opens and the supervisor decides to start it.
    pub fn register(&mut self, policy: Policy) -> SurfaceId {
        let id = self.supervisor.register(policy);
        self.subscribers.insert(id, self.store.next_subscriber_id());
        id
    }

    /// Forgets a surface entirely, returning whether it was registered.
    ///
    /// Does not tear its renderer down — call [`Host::tick`] or
    /// [`Host::request_suspend`] first if that is wanted.
    pub fn unregister(&mut self, id: SurfaceId) -> bool {
        self.subscribers.remove(&id);
        self.supervisor.unregister(id)
    }

    /// The subscriber this surface owns, or `None` if it is not registered.
    pub fn subscriber_for(&self, id: SurfaceId) -> Option<SubscriberId> {
        self.subscribers.get(&id).copied()
    }

    /// The surface's lifecycle state, or `None` if it is not registered.
    pub fn state(&self, id: SurfaceId) -> Option<SurfaceState> {
        self.supervisor.state(id)
    }
}
```

- [ ] **Step 4: Export from `lib.rs`**

Add `pub mod host;` and `pub use host::Host;`. Convert any plain-backtick `Host` references in the crate docs into intra-doc links.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p duet-host`
Expected: PASS — 8 passed.

- [ ] **Step 6: Commit**

```bash
git add crates/duet-host/src/
git commit -m "feat(host): add Host with the surface-to-subscriber mapping"
```

---

## Task 3: Event translation and action execution

The centre of the crate.

**Files:**
- Modify: `crates/duet-host/src/host.rs`

- [ ] **Step 1: Write the failing test**

Add inside `mod tests` in `crates/duet-host/src/host.rs`:

```rust
    #[test]
    fn opening_a_window_starts_the_surface_and_attaches_its_view() {
        let (mut h, b, rt) = host();
        let id = h.register(Policy::Never);
        let w = WindowId::new(1);

        h.handle_at(Instant(0), HostEvent::WindowOpened { surface: id, window: w });
        h.tick(Instant(0));

        assert_eq!(
            b.calls(),
            vec![
                BackendCall::StartRenderer(id),
                BackendCall::AttachView(id),
            ],
            "a cold start must create the renderer and then attach"
        );
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn a_successful_start_reports_ready_to_the_supervisor() {
        let (mut h, _b, rt) = host();
        let id = h.register(Policy::Never);
        h.handle_at(Instant(0), HostEvent::WindowOpened { surface: id, window: WindowId::new(1) });
        h.tick(Instant(0));
        assert_eq!(
            h.state(id),
            Some(SurfaceState::Live),
            "the host must close the loop by reporting Ready itself"
        );
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn a_failed_start_reports_failure_and_does_not_attach() {
        let (mut h, b, rt) = host();
        let id = h.register(Policy::Never);
        b.fail_next(BackendError::Unavailable("no display".to_string()));

        h.handle_at(Instant(0), HostEvent::WindowOpened { surface: id, window: WindowId::new(1) });
        h.tick(Instant(0));

        assert_eq!(
            b.calls(),
            vec![BackendCall::StartRenderer(id)],
            "a failed start must not be followed by an attach"
        );
        assert!(
            matches!(h.state(id), Some(SurfaceState::Failed(_))),
            "the failure must reach the supervisor, got {:?}",
            h.state(id)
        );
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn suspending_detaches_the_view_but_keeps_the_renderer() {
        let (mut h, b, rt) = host();
        let id = h.register(Policy::OnLastWindowClosed { grace_ms: 1_000 });
        let w = WindowId::new(1);
        h.handle_at(Instant(0), HostEvent::WindowOpened { surface: id, window: w });
        h.tick(Instant(0));

        h.handle_at(Instant(100), HostEvent::WindowClosed { surface: id, window: w });
        h.tick(Instant(100));

        assert_eq!(
            b.calls(),
            vec![
                BackendCall::StartRenderer(id),
                BackendCall::AttachView(id),
                BackendCall::DetachView(id),
            ],
            "suspend detaches only — Spike A measured that destroying is what frees memory"
        );
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn teardown_destroys_the_renderer_and_drops_the_subscriber() {
        let (mut h, b, rt) = host();
        let id = h.register(Policy::OnLastWindowClosed { grace_ms: 0 });
        let w = WindowId::new(1);
        let subscriber = h.subscriber_for(id).expect("registered surfaces have subscribers");

        h.handle_at(Instant(0), HostEvent::WindowOpened { surface: id, window: w });
        h.tick(Instant(0));
        // Clone the handle: holding a borrow of `h` here would conflict with
        // the `&mut h` needed by `handle_at`/`tick` below.
        let store = h.store_handle().clone();
        store
            .subscribe(subscriber, duet_core::Path::root())
            .expect("subscribe should succeed");

        h.handle_at(Instant(10), HostEvent::WindowClosed { surface: id, window: w });
        h.tick(Instant(10));
        h.tick(Instant(11));

        assert!(
            b.calls().contains(&BackendCall::DestroyRenderer(id)),
            "teardown must destroy the renderer, got {:?}",
            b.calls()
        );
        assert_eq!(
            store.drop_subscriber(subscriber).expect("query should succeed"),
            0,
            "the host must already have dropped this surface's subscriptions"
        );
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn resume_attaches_without_starting_a_new_renderer() {
        let (mut h, b, rt) = host();
        let id = h.register(Policy::OnLastWindowClosed { grace_ms: 10_000 });
        let w = WindowId::new(1);
        h.handle_at(Instant(0), HostEvent::WindowOpened { surface: id, window: w });
        h.tick(Instant(0));
        h.handle_at(Instant(100), HostEvent::WindowClosed { surface: id, window: w });
        h.tick(Instant(100));

        // Reopen well inside the grace window.
        h.handle_at(Instant(200), HostEvent::WindowOpened { surface: id, window: w });
        h.tick(Instant(200));

        let starts = b
            .calls()
            .iter()
            .filter(|c| matches!(c, BackendCall::StartRenderer(_)))
            .count();
        assert_eq!(
            starts, 1,
            "reattaching must not boot a second engine — Spike A measured that at ~180 ms"
        );
        assert_eq!(
            b.calls().last(),
            Some(&BackendCall::AttachView(id)),
            "resume ends in an attach"
        );
        rt.shutdown().expect("shutdown should succeed");
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p duet-host`
Expected: FAIL — `no method named handle_at found for struct Host`.

- [ ] **Step 3: Write the implementation**

Add to `impl<B: WindowBackend> Host<B>`:

```rust
    /// Borrows the store handle, for callers that need to read or subscribe.
    pub fn store_handle(&self) -> &StoreHandle {
        &self.store
    }

    /// Forwards a platform event to the supervisor.
    pub fn handle_at(&mut self, now: Instant, event: HostEvent) {
        self.supervisor.handle_at(now, event);
    }

    /// Advances the supervisor and performs whatever it decides.
    ///
    /// Returns the actions that were executed, which is what tests and logs
    /// assert on. An action whose backend call fails is still returned — the
    /// failure is reported to the supervisor separately, as a
    /// `HostEvent::Failed`.
    pub fn tick(&mut self, now: Instant) -> Vec<SurfaceAction> {
        let actions = self.supervisor.tick(now);
        for action in &actions {
            self.perform(*action, now);
        }
        actions
    }

    /// Executes one action, closing the loop back to the supervisor.
    ///
    /// The supervisor cannot know whether a renderer actually came up, so the
    /// host reports `Ready` or `Failed` itself. Without that, a surface told to
    /// start would sit in `Starting` forever and its memory would never be
    /// reclaimed.
    fn perform(&mut self, action: SurfaceAction, now: Instant) {
        let id = action.surface();
        let outcome = match action {
            SurfaceAction::Start(_) => self
                .backend
                .start_renderer(id)
                .and_then(|()| self.backend.attach_view(id)),
            SurfaceAction::Resume(_) => self.backend.attach_view(id),
            SurfaceAction::Suspend(_) => self.backend.detach_view(id),
            SurfaceAction::Teardown(_) => {
                // Drop subscriptions before destroying the renderer, so the
                // store cannot deliver to a surface that is going away.
                self.drop_subscriptions(id);
                self.backend.destroy_renderer(id)
            }
        };

        match (action, outcome) {
            // Only a start needs confirming: Resume moves to Live immediately,
            // and Suspend/Teardown have already transitioned.
            (SurfaceAction::Start(_), Ok(())) => {
                self.supervisor.handle_at(now, HostEvent::Ready(id));
            }
            (_, Err(e)) => {
                self.supervisor
                    .handle_at(now, HostEvent::Failed(id, e.to_string()));
            }
            (_, Ok(())) => {}
        }
    }

    /// Drops every store subscription belonging to a surface.
    ///
    /// `duet_supervisor::SurfaceAction::Teardown` documents this as the host's
    /// obligation: the supervisor has no store handle, and a missed drop leaves
    /// the store computing and delivering notifications for a renderer that no
    /// longer exists.
    ///
    /// A store error here is deliberately swallowed — if the runtime is already
    /// gone there is nothing to drop and nothing to recover.
    fn drop_subscriptions(&self, id: SurfaceId) {
        if let Some(subscriber) = self.subscribers.get(&id) {
            let _ = self.store.drop_subscriber(*subscriber);
        }
    }

    /// Asks a surface to suspend regardless of its policy, performing whatever
    /// the supervisor decides.
    pub fn request_suspend(&mut self, id: SurfaceId, now: Instant) -> Option<SurfaceAction> {
        let action = self.supervisor.request_suspend(id, now)?;
        self.perform(action, now);
        Some(action)
    }

    /// Asks a surface to resume regardless of its policy, performing whatever
    /// the supervisor decides.
    pub fn request_resume(&mut self, id: SurfaceId, now: Instant) -> Option<SurfaceAction> {
        let action = self.supervisor.request_resume(id, now)?;
        self.perform(action, now);
        Some(action)
    }
```

Extend the test module's imports to cover everything the new tests use:

```rust
    use crate::backend::{BackendCall, BackendError, RecordingBackend};
    use duet_core::{Instant, Policy, SurfaceState, Value};
    use duet_runtime::{NullSink, Runtime};
    use duet_supervisor::{HostEvent, SurfaceId, WindowId};
```

Note the ordering in the `Teardown` arm: subscriptions are dropped **before** the renderer is destroyed. Reversing it opens a window in which the store can still produce notifications for a surface whose renderer is already gone.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p duet-host`
Expected: PASS — 14 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/duet-host/src/
git commit -m "feat(host): translate events and execute supervisor actions"
```

---

## Task 4: Full host-loop integration tests

**Files:**
- Create: `crates/duet-host/tests/orchestration.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/duet-host/tests/orchestration.rs`:

```rust
//! Full orchestration loops, driven only through the public API.

use duet_core::{Instant, Path, Policy, SurfaceState, Value};
use duet_host::{BackendCall, Host, RecordingBackend, WindowBackend};
use duet_runtime::{NullSink, Runtime};
use duet_supervisor::{HostEvent, SurfaceAction, WindowId};

fn setup() -> (Host<RecordingBackend>, RecordingBackend, Runtime) {
    let rt = Runtime::spawn(
        Value::map([("editor", Value::map([("zoom", Value::Float(1.0))]))]),
        NullSink,
    );
    let backend = RecordingBackend::new();
    let host = Host::new(rt.handle(), backend.clone());
    (host, backend, rt)
}

/// Runs the host for `ticks` steps of `step` milliseconds, returning every
/// action performed.
///
/// This is the loop the real event loop will run. Earlier phases of this
/// project twice shipped a defect that only appeared *after* the first action —
/// a policy that oscillated forever — because the tests stopped one tick too
/// early. Running the loop and pinning exact totals is what catches that.
fn run(host: &mut Host<RecordingBackend>, ticks: u64, step: u64) -> Vec<SurfaceAction> {
    let mut all = Vec::new();
    for i in 0..ticks {
        all.extend(host.tick(Instant(i * step)));
    }
    all
}

#[test]
fn a_window_opening_and_closing_drives_a_full_renderer_lifecycle() {
    let (mut h, b, rt) = setup();
    let id = h.register(Policy::OnLastWindowClosed { grace_ms: 1_000 });
    let w = WindowId::new(1);

    h.handle_at(Instant(0), HostEvent::WindowOpened { surface: id, window: w });
    h.tick(Instant(0));
    assert_eq!(h.state(id), Some(SurfaceState::Live));

    h.handle_at(Instant(500), HostEvent::WindowClosed { surface: id, window: w });
    let actions = run(&mut h, 6, 500);

    assert_eq!(
        b.calls(),
        vec![
            BackendCall::StartRenderer(id),
            BackendCall::AttachView(id),
            BackendCall::DetachView(id),
            BackendCall::DestroyRenderer(id),
        ],
        "the full lifecycle must be start, attach, detach, destroy — exactly once each"
    );
    assert!(
        actions.iter().any(|a| a.reclaims_memory()),
        "the loop must reach the action that actually frees memory"
    );
    assert_eq!(h.state(id), Some(SurfaceState::Cold));
    rt.shutdown().expect("shutdown should succeed");
}

#[test]
fn the_loop_settles_and_does_not_oscillate() {
    // Two earlier defects in this project made surfaces cycle forever. Both
    // were invisible to tests that stopped after the first action.
    let (mut h, b, rt) = setup();
    let id = h.register(Policy::OnHidden { grace_ms: 500 });
    let w = WindowId::new(1);

    // Open but never shown: hidden from the start.
    h.handle_at(Instant(0), HostEvent::WindowOpened { surface: id, window: w });
    let actions = run(&mut h, 40, 250);

    assert_eq!(
        actions.len(),
        0,
        "a permanently hidden window must never start a renderer, got {actions:?}"
    );
    assert_eq!(b.calls(), vec![], "and must never touch the platform");
    assert_eq!(h.state(id), Some(SurfaceState::Cold));
    rt.shutdown().expect("shutdown should succeed");
}

#[test]
fn two_surfaces_are_orchestrated_independently() {
    let (mut h, b, rt) = setup();
    let flutter = h.register(Policy::OnLastWindowClosed { grace_ms: 500 });
    let webview = h.register(Policy::Never);
    let wf = WindowId::new(1);
    let ww = WindowId::new(2);

    h.handle_at(Instant(0), HostEvent::WindowOpened { surface: flutter, window: wf });
    h.handle_at(Instant(0), HostEvent::WindowOpened { surface: webview, window: ww });
    h.tick(Instant(0));

    h.handle_at(Instant(100), HostEvent::WindowClosed { surface: flutter, window: wf });
    run(&mut h, 6, 200);

    assert!(
        b.calls().contains(&BackendCall::DestroyRenderer(flutter)),
        "the policy-governed surface must be torn down"
    );
    assert!(
        !b.calls().contains(&BackendCall::DestroyRenderer(webview)),
        "a Never-policy surface must survive"
    );
    assert_eq!(h.state(webview), Some(SurfaceState::Live));
    rt.shutdown().expect("shutdown should succeed");
}

#[test]
fn teardown_drops_only_the_torn_down_surfaces_subscriptions() {
    // Dropping the wrong surface's subscriptions would silently stop delivering
    // to a live renderer, and the two surfaces are separate guests.
    let (mut h, _b, rt) = setup();
    let doomed = h.register(Policy::OnLastWindowClosed { grace_ms: 0 });
    let survivor = h.register(Policy::Never);
    let store = h.store_handle().clone();

    let doomed_sub = h.subscriber_for(doomed).expect("registered");
    let survivor_sub = h.subscriber_for(survivor).expect("registered");
    store.subscribe(doomed_sub, Path::root()).expect("subscribe");
    store.subscribe(survivor_sub, Path::root()).expect("subscribe");

    let w = WindowId::new(1);
    h.handle_at(Instant(0), HostEvent::WindowOpened { surface: doomed, window: w });
    h.tick(Instant(0));
    h.handle_at(Instant(10), HostEvent::WindowClosed { surface: doomed, window: w });
    run(&mut h, 4, 10);

    assert_eq!(
        store.drop_subscriber(doomed_sub).expect("query"),
        0,
        "the torn-down surface's subscriptions must already be gone"
    );
    assert_eq!(
        store.drop_subscriber(survivor_sub).expect("query"),
        1,
        "the surviving surface's subscription must be untouched"
    );
    rt.shutdown().expect("shutdown should succeed");
}

#[test]
fn a_backend_failure_leaves_the_surface_failed_and_the_loop_stable() {
    let (mut h, b, rt) = setup();
    let id = h.register(Policy::Never);
    b.fail_next(duet_host::BackendError::Unavailable("no display".to_string()));

    h.handle_at(Instant(0), HostEvent::WindowOpened { surface: id, window: WindowId::new(1) });
    let actions = run(&mut h, 20, 100);

    assert!(
        matches!(h.state(id), Some(SurfaceState::Failed(_))),
        "got {:?}",
        h.state(id)
    );
    assert_eq!(
        actions.len(),
        1,
        "a failed surface must be attempted once and then left alone, got {actions:?}"
    );
    rt.shutdown().expect("shutdown should succeed");
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p duet-host --test orchestration`
Expected: PASS — 5 passed. If a name is missing from a crate root, add the re-export — the integration test may only use public APIs, so a missing export is a real finding. Report it.

`StoreHandle` must be `Clone` for `h.store_handle().clone()`; it is. If any other type needs `Clone`, report rather than adding it blindly.

- [ ] **Step 3: Commit**

```bash
git add crates/duet-host/tests/
git commit -m "test(host): pin full orchestration loops"
```

---

## Task 5: Coverage gate and CI

**Files:**
- Modify: `.github/workflows/duet.yml` only if needed

- [ ] **Step 1: Measure coverage**

Run: `cargo llvm-cov -p duet-host --summary-only`

`cargo-llvm-cov` 0.8.7 is already installed. This forces an instrumented rebuild taking a few minutes — be patient.

Report the real per-file numbers. If any file is below 90% line coverage, read the report and add tests for those branches. **Do not lower the threshold.** If a line is genuinely unreachable, say so explicitly rather than contorting a test.

- [ ] **Step 2: Confirm the workspace gate still passes**

Run: `cargo llvm-cov --workspace --locked --fail-under-lines 90`
Expected: exit 0. Report the workspace total.

- [ ] **Step 3: Verify CI covers the new crate**

`.github/workflows/duet.yml` runs `--workspace` for every step, so the new crate should be gated automatically. **Read the file and confirm.** If any step names a specific crate, fix it.

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
git commit -m "ci: gate duet-host alongside the rest of the workspace"
```

---

## Done criteria

- [ ] `cargo test --workspace` passes — report exact counts per crate
- [ ] `cargo test --workspace -- --test-threads=1` passes with identical counts
- [ ] `cargo llvm-cov --workspace --fail-under-lines 90` exits 0
- [ ] `cargo clippy --workspace --all-targets --locked -- -D warnings` clean
- [ ] `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps` clean
- [ ] `cargo fmt --all -- --check` clean
- [ ] **No platform crates** — `cargo tree -p duet-host` shows only the three `duet-*` path dependencies
- [ ] `duet-core`, `duet-runtime` and `duet-codec` unchanged; `duet-supervisor` changed **only** by the additive `SurfaceId::from_raw` — verify with `git diff main -- crates/duet-supervisor`
- [ ] No `unwrap`/`expect` in non-test code
- [ ] The orchestration loop **settles** — `the_loop_settles_and_does_not_oscillate` passes with an exact count of zero

## What Phase 2b-2 deliberately does not build

- **The `tao`/`wry` backend.** 2b-3, behind `WindowBackend`, largely transcribed from Spike B's working code.
- **The `EventLoopProxy` sink.** Ships with the real backend, since it needs an event loop to marshal onto. The 2a review already verified it compiles against real `tao` 0.36.
- **Window creation.** The backend creates renderers; actual `tao` windows arrive with it.
- **The `Starting`-gap notification buffer.** `duet-runtime`'s docs record that it belongs there as a `Sink` adapter; it needs a readiness signal, which only exists once a real backend reports one.
