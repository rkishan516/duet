# Phase 2b-3 — `duet-backend-macos` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement `duet-host`'s `WindowBackend` and `duet-runtime`'s `Sink` against real `tao` windows, a real Flutter engine and a real `wry` webview — and **prove the framework's headline claim by measuring reclaimed memory end to end.**

**Architecture:** A `MacBackend` owns `tao` windows and per-surface renderers, implementing `WindowBackend`. A `ProxySink` wraps `tao`'s `EventLoopProxy` to marshal notifications onto the UI thread. Both are transcriptions of code Spike A and Spike B already proved on this machine, adapted to the traits the merged crates define.

**Tech Stack:** Rust 1.92, edition 2024. `tao` 0.36, `wry` 0.56, `objc2` 0.6.4 (**with `catch-all`**), `objc2-app-kit` 0.3.2, `objc2-foundation` 0.3.2, plus the four `duet-*` crates.

**Reference:** spec §6.1 (embedding), §6.2 (threading), §6.4 (multi-view).
**Evidence:** `docs/superpowers/spikes/2026-08-04-phase0-findings.md`. Working reference code: `spikes/spike-a-macos/` (engine embedding) and `spikes/spike-b-macos/` (run loop coexistence, `EventLoopProxy`, webview).

---

## Read this before anything else: what can and cannot be verified here

Spike A established that **this machine has no reachable on-screen WindowServer for spawned processes.** Windows are created and render, but nothing appears on a physical display and no human can interact with them.

**Verifiable here:**
- It compiles and links against real `tao`, `wry` and `FlutterMacOS.framework`.
- A Flutter view renders — proven by **in-process rasterization** (`cacheDisplayInRect:toBitmapImageRep:`), which Spike A used to capture real PNGs of the counter app.
- `ProxySink` delivers — Spike B measured 709/709 events over 180 s with zero loss.
- **RSS. This is the important one.** Spike A measured a real Flutter surface at 148 MB (engine, no view), 223 MB (view attached), and **104 MB after `shutDownEngine`**. Memory is measurable without a display, so a full lifecycle driven through the real `Host` can prove the framework actually reclaims memory. **Every phase so far has asserted that structurally; this is the first that can measure it.**

**Not verifiable here, and must not be claimed:**
- Real keyboard or mouse input. Spike B found synthetic events reach a Flutter view but **not** a `WKWebView`; that asymmetry is unexplained and needs real hardware.
- Anything a human would judge by looking at a screen.

State this honestly in `FINDINGS`-style reporting. A "cannot verify here" is a legitimate result; a fabricated pass is not.

---

## Background: the exact API sequence Spike A confirmed

Do **not** re-derive this. From the Phase 0 findings, verified by a running binary:

```
FlutterDartProject  initWithPrecompiledDartBundle:  <NSBundle of App.framework>
FlutterEngine       initWithName:project:allowHeadlessExecution:YES
FlutterEngine       runWithEntrypoint:nil                        -> BOOL
FlutterViewController initWithEngine:nibName:bundle:              (per view)
  .view -> NSView, addSubview into the tao NSWindow's contentView
detach = removeFromSuperview + drop the controller (engine holds it only weakly)
FlutterEngine       shutDownEngine
```

Five constraints that cost Spike A real time:

1. **`allowHeadlessExecution:YES`** is what lets the engine exist with zero views.
2. **Never use the `engine.viewController` property.** It is the legacy single-view path; reassigning it throws.
3. **One view per engine at a time.** A second concurrent `initWithEngine:` throws `NSInternalInconsistencyException — The engine already has a view controller for the implicit view`. Sequential detach-then-create works; simultaneous does not. **So: one engine per Flutter window.**
4. **`objc2`'s `catch-all` feature is mandatory.** Without it an Objective-C exception aborts the process with no message, no reason, no backtrace.
5. **Detach → recreate must cross a run-loop tick.** Doing both inside one synchronous callback races into constraint 3 even though the old controller was dropped.

Assets come from an `NSBundle` — macOS `FlutterDartProject` has no assets-path API. Point it at Flutter's `App.framework`, whose `Info.plist` carries `CFBundleIdentifier = io.flutter.flutter.app`.

`spikes/spike-a-macos/build.rs` already solves linking and rpath; copy it, keeping the `FLUTTER_MACOS_FRAMEWORK_DIR` override.

---

## The traits you are implementing

From `duet-host`:

```rust
pub enum Readiness { Ready, Pending }

pub trait WindowBackend {
    fn start_renderer(&mut self, surface: SurfaceId) -> Result<Readiness, BackendError>;
    fn attach_view(&mut self, surface: SurfaceId) -> Result<(), BackendError>;
    fn detach_view(&mut self, surface: SurfaceId) -> Result<(), BackendError>;
    fn destroy_renderer(&mut self, surface: SurfaceId) -> Result<(), BackendError>;
}
```

`Readiness::Pending` exists precisely for this phase: a Flutter boot takes ~180 ms and blocking the main thread would freeze every window. **Confirm which one a real engine warrants** — Spike A's `runWithEntrypoint:` returns synchronously once the isolate is running, so `Ready` may be honest for Flutter while a webview load is genuinely `Pending`. Report what you find.

From `duet-runtime`:

```rust
pub trait Sink: Send + 'static {
    fn deliver(&self, batch: Vec<Notification>) -> Result<(), SinkError>;
}
```

`deliver` runs on the **core thread** and must not block or serialize — post the batch and return. The 2a review verified `EventLoopProxy` satisfies this trait unchanged by compiling it against real `tao` 0.36.

---

## Standing quality bar

Adjusted for a crate that cannot reach high coverage here.

- Every public item documented, including every variant and field; `#![deny(missing_docs)]`.
- `# Errors` sections on every `Result` return.
- **No `unwrap`/`expect`/`panic!`/`unreachable!` in non-test code.** An ObjC failure must become a `BackendError`, not an abort.
- **This crate is exempt from the 90% coverage gate** — most of it cannot run without a display. Exclude it from the workspace gate explicitly rather than lowering the threshold for everything else.
- **Never claim a pass you did not observe.** Spikes A and B both recorded "cannot verify here" results and were more valuable for it.

---

## File Structure

| File | Responsibility |
|---|---|
| `crates/duet-backend-macos/Cargo.toml` | Manifest; platform + `duet-*` deps |
| `crates/duet-backend-macos/build.rs` | Framework link path and rpath (copy Spike A's) |
| `crates/duet-backend-macos/src/lib.rs` | Crate docs, module decls, re-exports |
| `crates/duet-backend-macos/src/engine.rs` | Flutter engine and view controller lifetime |
| `crates/duet-backend-macos/src/backend.rs` | `MacBackend` implementing `WindowBackend` |
| `crates/duet-backend-macos/src/sink.rs` | `ProxySink` implementing `Sink` |
| `crates/duet-backend-macos/examples/lifecycle.rs` | The RSS proof — the phase's real deliverable |

---

## Task 1: Scaffold, linking, and `ProxySink`

`ProxySink` first because it is the one piece with no display dependency at all.

**Files:**
- Create: `crates/duet-backend-macos/{Cargo.toml,build.rs,src/lib.rs,src/sink.rs}`
- Modify: root `Cargo.toml`

- [ ] **Step 1: Add to the workspace**

Extend `members` with `"crates/duet-backend-macos"`. Leave `exclude = ["spikes"]` alone.

- [ ] **Step 2: Manifest**

```toml
[package]
name = "duet-backend-macos"
description = "macOS backend for Duet: tao windows, a Flutter engine and a wry webview"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[dependencies]
duet-core = { path = "../duet-core" }
duet-runtime = { path = "../duet-runtime" }
duet-supervisor = { path = "../duet-supervisor" }
duet-host = { path = "../duet-host" }
tao = "0.36"
wry = "0.56"
objc2 = { version = "0.6.4", features = ["catch-all"] }
objc2-app-kit = "0.3.2"
objc2-foundation = "0.3.2"
```

The `catch-all` feature is **not optional** — see constraint 4 above.

- [ ] **Step 3: `build.rs`**

Copy `spikes/spike-a-macos/build.rs` verbatim. It emits the framework search path, links `FlutterMacOS`, and embeds an rpath so `cargo run` works without `DYLD_FRAMEWORK_PATH`. Keep the `FLUTTER_MACOS_FRAMEWORK_DIR` env override and the existence check that panics with an actionable message — a build script is the right place for that panic, unlike library code.

- [ ] **Step 4: Write the failing test for `ProxySink`**

Create `crates/duet-backend-macos/src/sink.rs`:

```rust
//! Marshals store notifications onto the UI thread.

#[cfg(test)]
mod tests {
    use super::*;
    use duet_core::{Notification, Patch, Path, SubscriberId, SubscriptionId, Value};
    use duet_runtime::Sink;
    use tao::event_loop::EventLoopBuilder;

    fn note() -> Notification {
        Notification {
            subscriber: SubscriberId(1),
            subscription: SubscriptionId(1),
            patch: Patch {
                path: Path::parse("editor.zoom").expect("test path should parse"),
                value: Value::Float(1.0),
            },
        }
    }

    #[test]
    fn delivering_to_a_closed_loop_reports_closed_rather_than_panicking() {
        // Build a loop, take a proxy, drop the loop. A UI that has exited must
        // not take the core thread down with it — `duet-runtime` treats a
        // closed sink as non-fatal, and this is the shape that produces it.
        let sink = {
            let event_loop = EventLoopBuilder::<DuetEvent>::with_user_event().build();
            ProxySink::new(event_loop.create_proxy())
        };
        assert_eq!(sink.deliver(vec![note()]), Err(duet_runtime::SinkError::Closed));
    }

    #[test]
    fn proxy_sink_is_send_and_static() {
        // `Sink` requires `Send + 'static` because the core thread owns it.
        fn assert_sink<S: Sink>() {}
        assert_sink::<ProxySink>();
    }
}
```

**Note:** building a `tao` event loop off the main thread panics on macOS. If this test cannot run in a test harness thread, mark it `#[ignore]` with a comment explaining why and **report that** — do not delete it or fake the assertion.

- [ ] **Step 5: Run test to verify it fails**

Run: `cargo test -p duet-backend-macos`
Expected: FAIL — `cannot find type ProxySink in this scope`.

- [ ] **Step 6: Implement**

```rust
use duet_core::Notification;
use duet_runtime::{Sink, SinkError};
use tao::event_loop::EventLoopProxy;

/// A user event carried from the core thread to the UI thread.
#[derive(Debug)]
pub enum DuetEvent {
    /// A batch of store notifications to deliver to guests.
    Notifications(Vec<Notification>),
    /// Ask the host to run a supervisor tick.
    Tick,
}

/// Marshals notification batches onto the UI thread via `tao`'s proxy.
///
/// Spike B measured this mechanism at 709 events sent and 709 received over
/// 180 seconds with zero loss, driving both a Flutter platform channel and a
/// webview `evaluate_script` from a single handler.
#[derive(Debug)]
pub struct ProxySink {
    proxy: EventLoopProxy<DuetEvent>,
}

impl ProxySink {
    /// Wraps an event loop proxy.
    pub fn new(proxy: EventLoopProxy<DuetEvent>) -> Self {
        ProxySink { proxy }
    }
}

impl Sink for ProxySink {
    /// Posts the batch to the UI thread and returns immediately.
    ///
    /// Does no serialization: `deliver` runs on the core thread, so anything
    /// done here is head-of-line latency for every subsequent reader.
    ///
    /// # Errors
    ///
    /// [`SinkError::Closed`] once the event loop has exited. `duet-runtime`
    /// treats that as non-fatal — a dead UI must not take the store down.
    fn deliver(&self, batch: Vec<Notification>) -> Result<(), SinkError> {
        self.proxy
            .send_event(DuetEvent::Notifications(batch))
            .map_err(|_| SinkError::Closed)
    }
}
```

- [ ] **Step 7: Crate root**

Create `src/lib.rs` with `#![deny(missing_docs)]`, crate docs explaining that this is the macOS backend and what Spikes A and B proved, `pub mod sink;`, and `pub use sink::{DuetEvent, ProxySink};`.

**Do not** add `#![forbid(unsafe_code)]` — this crate calls Objective-C and needs `unsafe`. Say so in the crate docs, and note that every `unsafe` block must carry a `// SAFETY:` comment.

- [ ] **Step 8: Run and commit**

Run: `cargo test -p duet-backend-macos`
Expected: PASS (or the one test ignored with its reason reported).

```bash
git add Cargo.toml Cargo.lock crates/duet-backend-macos/
git commit -m "feat(backend-macos): add ProxySink over tao's EventLoopProxy"
```

---

## Task 2: Flutter engine lifetime

**Files:**
- Create: `crates/duet-backend-macos/src/engine.rs`
- Modify: `crates/duet-backend-macos/src/lib.rs`

- [ ] **Step 1: Port the engine wrapper from Spike A**

Read `spikes/spike-a-macos/src/main.rs`. It contains working, verified code for every call in the sequence above. Extract it into a `FlutterEngine` type with this shape:

```rust
/// Owns one Flutter engine and, at most, one attached view.
///
/// The engine is created headless and a view controller is attached separately,
/// which is what makes teardown independent of window lifetime.
///
/// **One view per engine.** A second concurrent `initWithEngine:` throws
/// `NSInternalInconsistencyException`; Spike A confirmed this, so Duet uses one
/// engine per Flutter window rather than the multi-view path the headers
/// appear to offer.
pub(crate) struct FlutterEngine { /* Retained handles */ }

impl FlutterEngine {
    /// Boots an engine with no view, from the assets in `app_framework`.
    ///
    /// # Errors
    /// [`BackendError::Unavailable`] if the bundle is missing or the engine
    /// declines to run.
    pub(crate) fn boot(app_framework: &str) -> Result<Self, BackendError>;

    /// Creates a view controller and adds its `NSView` to `window`'s content view.
    ///
    /// # Errors
    /// [`BackendError::Unavailable`] if the view could not be created or added.
    pub(crate) fn attach(&mut self, window: &tao::window::Window) -> Result<(), BackendError>;

    /// Removes the view from its superview and drops the controller.
    ///
    /// Spike A measured that this reclaims essentially nothing — 223 MB before
    /// and after. Only [`FlutterEngine::shutdown`] frees memory.
    pub(crate) fn detach(&mut self);

    /// Shuts the engine down. **This is what reclaims memory** — Spike A
    /// measured 223 MB before and 104 MB after.
    pub(crate) fn shutdown(&mut self);
}
```

Every `unsafe` block gets a `// SAFETY:` comment. Every ObjC failure becomes a `BackendError`, never a panic — `catch-all` turns ObjC exceptions into Rust panics, so wrap fallible calls and convert.

- [ ] **Step 2: Verify it builds**

Run: `cargo build -p duet-backend-macos`
Expected: success. A link error means `FLUTTER_MACOS_FRAMEWORK_DIR` is wrong — the build script's message says so.

There is no unit test here: booting an engine needs the framework and produces a real isolate. Task 4's example is the verification.

- [ ] **Step 3: Commit**

```bash
git add crates/duet-backend-macos/src/
git commit -m "feat(backend-macos): port the Flutter engine wrapper from Spike A"
```

---

## Task 3: `MacBackend`

**Files:**
- Create: `crates/duet-backend-macos/src/backend.rs`
- Modify: `crates/duet-backend-macos/src/lib.rs`

- [ ] **Step 1: Implement `WindowBackend`**

```rust
/// Owns `tao` windows and per-surface renderers, implementing
/// [`duet_host::WindowBackend`].
///
/// One Flutter engine per surface: Spike A confirmed an engine accepts only one
/// view controller at a time.
pub struct MacBackend { /* windows and engines by SurfaceId */ }
```

`start_renderer` boots an engine; `attach_view` attaches it to that surface's window; `detach_view` detaches; `destroy_renderer` shuts down and drops.

**Two constraints from Spike A that the implementation must respect:**

- **Detach → recreate must cross a run-loop tick.** Doing both synchronously races into the one-view-per-engine exception even though the controller was dropped. `duet-host` only ever issues one action per surface per `tick`, which naturally separates them — **verify that holds** and say so in a comment, because it is load-bearing and non-obvious.
- **Rapid cycling logs `Could not create the embedder backing store`.** Spike A saw this over 8 cycles with no crash and it remains unexplained. If you see it, record it; do not treat it as failure.

**Report what `start_renderer` should return.** `runWithEntrypoint:` returns once the isolate runs, so `Readiness::Ready` may be honest for Flutter. A `wry` webview load is asynchronous and warrants `Pending`. Decide per renderer kind and justify it.

- [ ] **Step 2: Verify it builds and passes clippy**

Run: `cargo build -p duet-backend-macos && cargo clippy -p duet-backend-macos --all-targets -- -D warnings`
Expected: both clean.

- [ ] **Step 3: Commit**

```bash
git add crates/duet-backend-macos/src/
git commit -m "feat(backend-macos): implement WindowBackend over tao and Flutter"
```

---

## Task 4: The RSS proof — this phase's real deliverable

**Files:**
- Create: `crates/duet-backend-macos/examples/lifecycle.rs`

This is the first time the framework's headline claim can be **measured** rather than asserted.

- [ ] **Step 1: Write the example**

An example, not a test: it needs the main thread and a real event loop, which `cargo test` cannot provide.

It must:

1. Build a `tao` event loop with `DuetEvent` as the user event.
2. Spawn a `Runtime` with a `ProxySink` over that loop's proxy.
3. Create a `Host` over the runtime's handle and a `MacBackend`.
4. Register one Flutter surface with `Policy::OnLastWindowClosed { grace_ms: 500 }`.
5. Open a window, tick, and let the host start the surface.
6. **Rasterize the Flutter view to a PNG** using Spike A's `cacheDisplayInRect:toBitmapImageRep:` approach — real proof of rendering, since no display is reachable.
7. Close the window and tick past the grace period so the host reaches `Teardown`.
8. **Sample RSS at every stage** using Spike A's `rss_kb()` helper, and print a table.
9. Assert, and print PASS or FAIL: **RSS after teardown must be at least 80 MB below RSS with the view attached.** Spike A measured a 119 MB drop (223 → 104); 80 MB is a generous floor that still cannot pass if teardown does nothing.

- [ ] **Step 2: Run it**

```bash
cargo run -p duet-backend-macos --example lifecycle
```

Report the **actual RSS table and the actual delta**. If the assertion fails, that is a finding about the framework, not the example — report it rather than adjusting the threshold.

The Flutter fixture is at `spikes/spike_app/build/macos/Build/Products/Debug/App.framework`. If it is missing, rebuild it:

```bash
cd spikes/spike_app && flutter build macos --debug
```

- [ ] **Step 3: Commit**

```bash
git add crates/duet-backend-macos/examples/
git commit -m "feat(backend-macos): measure reclaimed memory across a real lifecycle"
```

---

## Task 5: Exempt this crate from the coverage gate, and record findings

**Files:**
- Modify: `.github/workflows/duet.yml`
- Create: `crates/duet-backend-macos/FINDINGS.md`

- [ ] **Step 1: Exclude from the coverage gate**

Most of this crate cannot execute without a display, so it cannot reach 90%. **Exclude the crate; do not lower the threshold for everything else** — the other five crates are all above 96% and that bar should not move.

In `.github/workflows/duet.yml`, change the coverage step to:

```yaml
      - name: Test with coverage gate
        run: cargo llvm-cov --workspace --exclude duet-backend-macos --locked --fail-under-lines 90
```

Add a comment above it explaining why: the macOS backend needs a window server and a Flutter toolchain, neither of which CI has.

Also note that CI runs `ubuntu-latest`, so this crate **will not build there at all**. Either add `--exclude duet-backend-macos` to the clippy and test steps too, or gate the crate behind `[target.'cfg(target_os = "macos")'.dependencies]`. **Pick one, do it consistently, and say which** — a CI that silently stops covering a crate is worse than one that never covered it.

- [ ] **Step 2: Write `FINDINGS.md`**

Record, in the style of the Phase 0 spike findings:

- What was verified, with real numbers: the RSS table and delta, whether the rasterized PNG shows Flutter content, whether `ProxySink` delivered.
- **What could not be verified here and why** — real input, anything requiring a human at a display. Spike B's finding that synthetic input reaches Flutter but not `WKWebView` is still open and should be repeated here.
- What `start_renderer` returns for each renderer kind, and why.
- Whether `Could not create the embedder backing store` appeared.
- Anything contradicting the spec or the Phase 0 findings.

- [ ] **Step 3: Verify the rest of the workspace is unaffected**

```bash
cargo test --workspace --exclude duet-backend-macos --locked
cargo llvm-cov --workspace --exclude duet-backend-macos --locked --fail-under-lines 90
cargo fmt --all -- --check
```

All must pass. The other five crates must be **unchanged** — verify with `git diff --stat main -- crates/duet-core crates/duet-runtime crates/duet-codec crates/duet-supervisor crates/duet-host`.

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/duet.yml crates/duet-backend-macos/
git commit -m "ci: exclude the macOS backend from the coverage gate, with findings"
```

---

## Done criteria

- [ ] `cargo build -p duet-backend-macos` succeeds against real `tao`, `wry` and `FlutterMacOS.framework`
- [ ] `cargo run -p duet-backend-macos --example lifecycle` runs and prints its RSS table
- [ ] **The measured RSS drop after teardown is reported**, whether or not it clears 80 MB
- [ ] A rasterized PNG of the Flutter view exists and is inspected
- [ ] `cargo clippy -p duet-backend-macos --all-targets -- -D warnings` clean
- [ ] The other five crates are unchanged and still pass their gate
- [ ] `FINDINGS.md` records what could **not** be verified, as explicitly as what could
- [ ] No `unwrap`/`expect`/`panic!` in non-test, non-build-script code
- [ ] Every `unsafe` block carries a `// SAFETY:` comment

## What Phase 2b-3 deliberately does not build

- **Windows and Linux backends.** Phase 5. Linux remains the largest unretired risk and needs a machine that does not exist here.
- **The webview surface's IPC bridge.** `duet-codec` exists; wiring it to `wry` needs the message envelope, which is a later increment.
- **Hot reload.** Spike C proved it works (median 113 ms); the CLI that drives it is Phase 4.
- **The `Starting`-gap notification buffer.** It needs a real readiness signal, which `Readiness::Pending` now provides — so it becomes buildable after this phase, not during it.
