# Duet — Design Specification

**Date:** 2026-08-04
**Status:** Approved for planning
**Working name:** `duet` (two performers, one score). Rename before publish is cheap.

---

## 1. Purpose

Duet is a reusable open-source framework for desktop applications that render part of
their UI with Flutter and part with web technology, in **separate top-level OS windows**,
where either renderer can be torn down to reclaim its resources while the application
keeps running.

Three goals, in priority order:

1. **Resource reclamation.** An idle renderer releases its memory. This is the headline claim.
2. **Shared state.** Both renderers observe and mutate one source of truth.
3. **Minimal API.** The framework should be explainable in a short document.

### Governing principle

> **State survives teardown. Events don't.**

If a value must outlive a suspend, it belongs in the store. If it is a transient signal,
it is an event, delivered only to live surfaces and never queued. This single rule is the
framework's contract with its users and the rationale behind most decisions below.

### Non-goals

- Compositing Flutter and web content into a single window surface. Separate windows only.
- Mobile or web targets. Desktop only.
- Multi-device or offline state synchronisation. There is one host process.
- Direct renderer-to-renderer channels. See §5.4.

---

## 2. Decisions and rationale

| Decision | Choice | Rationale |
|---|---|---|
| Audience | Reusable OSS framework | Requires stable public API, docs, examples, generality |
| Host model | Rust host; Flutter and webview are guests | The only model where **both** sides can be torn down |
| State model | Observable store in Rust | Neither guest can own state if either can vanish |
| Teardown trigger | Declarative per-surface policy | Zero-config default, tunable when it matters |
| Type strategy | Rust-first + codegen to Dart and TS | Rust is already the source of truth; one place to look |
| Dev loop | Full hot reload parity in v1 | Flutter developers will not adopt a framework without it |
| Platforms | macOS, Windows, Linux in v1 | Full desktop story at launch |

The host-model decision is load-bearing. Because either guest may be destroyed at any
moment, neither can hold authoritative state, so state must live in the host. Every other
structural choice follows from this.

### Prior art to mine, not reinvent

- **NativeShell** (Matej Knopp) — Rust-hosted Flutter with custom windowing. Archived, but
  the closest existing precedent for the host model.
- **irondash** (`irondash_engine_context`, `irondash_message_channel`, `irondash_run_loop`) —
  actively maintained; solves engine-handle access, typed Rust↔Dart messaging, and run-loop
  integration. Assumes Flutter is the host, so it cannot be used wholesale, but the
  techniques transfer.
- **tauri-specta** — the schema-extraction approach adopted in §7.
- **Tauri v2** — separates `Window` from `Webview`, making a window with no webview a
  first-class construct. That is precisely the hole a Flutter view is parented into.

---

## 3. Architecture

```
┌──────────────────────────────────────────────────────────┐
│ Rust host process (owns app lifecycle + all OS windows)  │
│                                                          │
│  ┌────────────────────────────────────────────────────┐  │
│  │ CORE — no platform deps, fully unit-testable       │  │
│  │   Store      typed observable state tree           │  │
│  │   Router     commands (req/resp) + events (pubsub) │  │
│  │   Surfaces   lifecycle state machine + policy      │  │
│  └───────────────┬────────────────────┬───────────────┘  │
│                  │                    │                  │
│      ┌───────────▼──────┐   ┌─────────▼────────┐         │
│      │ FlutterSurface   │   │ WebviewSurface   │         │
│      │ engine + views   │   │ wry webview      │         │
│      └───────────┬──────┘   └─────────┬────────┘         │
│                  │                    │                  │
│  ┌───────────────▼────────────────────▼───────────────┐  │
│  │ PLATFORM — windows (tao), run loop, view parenting │  │
│  │   macOS NSView  ·  Windows HWND  ·  Linux GtkWidget│  │
│  └────────────────────────────────────────────────────┘  │
└──────────────────────────────────────────────────────────┘
```

### 3.1 Window ownership

Windows are owned by the host, never by a surface. Consequently window geometry, position,
visibility, and identity are host state and **survive teardown without app-author effort**.
A suspended window reappears at exactly the right size and position.

### 3.2 Crate layout

| Crate | Responsibility | Platform deps |
|---|---|---|
| `duet-core` | Store, router, lifecycle state machine, policy | none |
| `duet-platform` | Window creation, run loop, view parenting | all 3 |
| `duet-flutter` | Engine embedding, view attach/detach, message channel | all 3 |
| `duet-webview` | Tauri v2 webview surface + IPC bridge | via Tauri |
| `duet-codegen` | Rust schema → Dart + TS emitters | none |
| `duet-cli` | `dev` / `build`, `flutter assemble`, watch, VM service | none |
| `duet` (pub.dev) | Dart client: store bindings, command invocation | — |
| `@duet/client` (npm) | TS client: store bindings, command invocation | — |

`duet-core` having zero platform dependencies is deliberate: store semantics, patch
computation, the lifecycle state machine, and policy evaluation are all testable with plain
`cargo test` on any machine. The untestable-in-CI native work is confined to three thin
crates.

---

## 4. Shared state

### 4.1 Authoring

```rust
#[derive(SharedState)]
pub struct AppState {
    pub user: Option<User>,
    pub documents: Vec<Document>,
    pub editor: EditorState,
}
```

Codegen emits typed path handles, not stringly-typed accessors:

```dart
final zoom = store.editor.zoom.watch();   // ValueListenable<double>
store.editor.zoom.set(1.5);
```

```ts
const zoom = useStore(store.editor.zoom);
store.editor.zoom.set(1.5);
```

### 4.2 Subscription matching

A subscriber at path `P` is notified when a write lands at path `W` and either path is a
prefix of the other.

- Write `editor.zoom` → notifies subscribers at `editor.zoom`, `editor`, and root.
- Write `editor.zoom` → does **not** notify a subscriber at `editor.theme`.
- Write `editor` (whole struct) → notifies subscribers at `editor.zoom`, since their value
  may have changed.

### 4.3 Wire protocol

Notifications carry a minimal patch `(changed_path, new_value_at_that_path)`, not the whole
subtree. Each client maintains a local mirror of its subscribed paths and applies patches
to it. On first subscription — and on every resume from `Cold` — the host sends one full
snapshot, then patches thereafter.

Without this, a subscriber watching a 10,000-item `documents` list would receive all 10,000
items whenever one title changed. With it, they receive one string.

**v1 limitation:** `Vec` insert and remove are modelled as whole-vector replacement. Only
`vec[i].field` writes produce narrow patches. Collection deltas are a deliberate follow-up.

### 4.4 Write semantics

Writes are **authoritative round-trip, not optimistic**. A guest's `set()` travels to the
host, is applied on the core thread, and returns to every subscriber — including the writer —
as a patch.

This is in-process, so the cost is microseconds. In exchange there is exactly one ordering
of writes and no reconciliation logic anywhere in the system. Optimistic local echo may be
added as opt-in later if a real use case demands it.

Conflict resolution is last-write-wins, serialized by the core thread.

---

## 5. Surface lifecycle

### 5.1 States

```
     ┌──────┐   start    ┌──────────┐   ready   ┌────────┐
     │ Cold │───────────▶│ Starting │──────────▶│  Live  │
     └──────┘            └──────────┘           └────────┘
        ▲                                            │
        │  grace expires  ┌────────────┐   suspend   │
        └─────────────────│ Suspending │◀────────────┘
                          └────────────┘
                                │ resume (cancels teardown — cheap)
                                └──────────▶ Live

     ┌──────────────┐
     │ Failed(why)  │ ◀── from any state; host survives, retryable
     └──────────────┘
```

- **Cold** — no engine, no webview, no renderer process. The store retains everything.
- **Starting** — engine booting or webview creating. Incoming requests are queued.
- **Live** — attached, rendering, receiving events.
- **Suspending** — grace period. A resume here cancels the teardown rather than paying a
  full engine boot. This exists specifically to prevent thrash when a user closes and
  immediately reopens a window.
- **Failed(reason)** — creation failed or the guest crashed. The host stays alive.

`Failed` is published into the store, which yields a useful property: **if the webview
renderer crashes, the Flutter side can render the error UI**, and vice versa. Surface health
is just state, so the surviving side already knows.

> **Spike A finding (2026-08-04): `Suspending` is a latency state, not a memory state.**
> Measured RSS for the Flutter side on macOS:
>
> | Point | RSS |
> |---|---|
> | Process start | 14 MB |
> | Engine booted, **no view** | 148 MB |
> | View attached, rendering | 223 MB |
> | **View detached, engine alive** (`Suspending`) | **223 MB** |
> | **After `shutDownEngine`** (`Cold`) | **104 MB** |
>
> Detaching the view reclaims essentially nothing — the engine, its isolate, and its caches
> are the entire footprint, not the view. Only reaching `Cold` reclaims real memory, ~125 MB
> here.
>
> The state machine is unchanged and correct, but the *emphasis* in this document was wrong:
> the grace period buys responsiveness, not footprint, and every second of grace is a second
> of full memory retention. **The 5 s default in §5.2 should be re-tuned against measured
> engine boot cost** rather than assumed. Spike A's timings: engine allocation to
> `runWithEntrypoint` returning was ~120 ms, and view-controller creation to attached was
> ~60 ms — so roughly 180 ms from cold to a live view, on a warm filesystem cache with a debug
> build. That suggests a default well under 5 s is affordable, but the number to tune against
> is a release-build measurement on a cold cache, which Phase 3 should take.
>
> Two further constraints on this state machine, also from Spike A: `Suspending → Cold →
> Starting` **cannot be driven synchronously** — teardown and re-create must be separated by a
> run-loop tick or the engine throws; and rapid detach/recreate cycling logs
> `Could not create the embedder backing store`, which the Phase 3 soak test must investigate.

### 5.2 Policy

```rust
enum Policy {
    OnLastWindowClosed { grace: Duration },   // default: 5s
    OnHidden { grace: Duration },
    IdleTimeout(Duration),
    Never,
}
```

Manual `surface.suspend()` and `surface.resume()` are always available and always override
policy.

Policy evaluation lives in `duet-core` as a **pure function** of
`(visible_windows, last_interaction, now) -> desired_state`. No clock injection, no OS
mocking — the caller passes `now`. This makes the most behaviourally subtle part of the
framework trivially unit-testable.

For `IdleTimeout`, "interaction" is defined as any input event, command invocation, or
store write originating from that surface.

### 5.3 What survives

| Survives `Cold` | Dies at `Cold` |
|---|---|
| Everything in the store | Dart heap / JS heap |
| Window geometry, position, visibility | Scroll offsets, in-flight animations |
| Surface health and failure reason | Unsubmitted form fields, open dropdowns |
| Host-side effects of in-flight commands | Command *responses* to the dying guest |

A command already executing host-side **runs to completion** even if its caller is being
torn down; only response delivery is dropped. Otherwise a teardown could leave the store
half-written.

### 5.4 No guest-to-guest channel

Flutter never talks to the webview directly. To make the peer react, a guest writes state
or emits an event and the host routes it.

This is not purity for its own sake: **the peer may be `Cold`**, in which case a direct
channel would have no endpoint. Routing through the host means "write state" behaves
identically whether the peer is live, suspended, or has never started.

---

## 6. Native embedding, threading, transport

### 6.1 Platform seam

```rust
trait WindowHost  { fn create(&self, opts: WindowOptions) -> Result<WindowHandle>; /* … */ }

trait FlutterHost {
    fn create_engine(&self, cfg: &EngineConfig) -> Result<EngineHandle>;
    fn shutdown_engine(&self, e: EngineHandle);
    fn create_view(&self, e: EngineHandle) -> Result<ViewHandle>;
    fn attach_view(&self, v: ViewHandle, w: WindowHandle) -> Result<()>;
    fn detach_view(&self, v: ViewHandle);
    fn send_message(&self, e: EngineHandle, channel: &str, bytes: &[u8]);
}

trait WebviewHost { /* create / destroy / eval / ipc */ }
```

| | Engine API | View type | Parented via |
|---|---|---|---|
| macOS | `FlutterMacOS.framework` — `FlutterEngine`, `FlutterViewController` | `NSView` | `addSubview` on the window's `contentView` (via `objc2` / `objc2-app-kit`) |
| Windows | `flutter_windows.h` — `FlutterDesktopEngineCreate`, `FlutterDesktopViewControllerCreate` | `HWND` | `SetParent` + `WS_CHILD` restyle |
| Linux | `flutter_linux` GTK — `fl_engine_new`, `fl_view_new` | `GtkWidget*` | `gtk_container_add`; tao already uses GTK3 here |

**macOS: CONFIRMED by Spike A** (2026-08-04). The engine-first path exists and works. Verified
sequence, from a running Rust binary:

```
FlutterDartProject  initWithPrecompiledDartBundle:  <NSBundle of App.framework>
FlutterEngine       initWithName:project:allowHeadlessExecution:YES
FlutterEngine       runWithEntrypoint:nil                        -> BOOL
FlutterViewController initWithEngine:nibName:bundle:              (per view)
  .view -> NSView, addSubview into the tao NSWindow's contentView
detach = removeFromSuperview + drop the controller (engine holds it only weakly)
FlutterEngine       shutDownEngine
```

`allowHeadlessExecution:YES` is what lets the engine exist with zero views. The
`engine.viewController` property must **not** be used — it is the legacy single-view path and
reassigning it throws.

Two corrections to this section, both from Spike A:

- **Assets must come from an `NSBundle`.** macOS `FlutterDartProject` has no
  `initWithAssetsPath:` — only `initWithPrecompiledDartBundle:`. §8's tooling must produce a
  real `.app` bundle or an `App.framework`-shaped directory, not a bare `flutter_assets` copy.
- **`objc2`'s `catch-all` feature is mandatory** for `duet-flutter`. Without it an
  Objective-C exception crossing into Rust aborts the process with no message, no reason and no
  backtrace.

**Still open for Phase 0:** Windows and Linux engine-first signatures remain unverified.

See `docs/superpowers/spikes/2026-08-04-phase0-findings.md`.

### 6.2 Threading

Hard constraint: Flutter's platform thread, tao's event loop, and the webview all require
the OS main thread. The main thread must therefore never perform store work.

```
┌─ MAIN / UI THREAD ────────────────────────────────────┐
│  tao event loop · Flutter platform thread · webview   │
└────────┬──────────────────────────────────▲───────────┘
         │ post write                       │ post patch
         │                                  │ (tao EventLoopProxy)
┌────────▼──────────────────────────────────┴───────────┐
│  CORE THREAD — store mutations, patch computation     │
│  serialized, short work only, never blocks            │
└────────┬──────────────────────────────────▲───────────┘
         │ dispatch                         │ store access
┌────────▼──────────────────────────────────┴───────────┐
│  TASK POOL (tokio) — user `#[command]` bodies, async  │
└───────────────────────────────────────────────────────┘
```

Two consequences:

- User command bodies may be slow without janking the UI. They run on the pool and reach
  the store through the same queue as everyone else.
- **One** notification mechanism, not three. Rather than `dispatch_async` on macOS,
  `PostMessage` on Windows, and `g_idle_add` on Linux, all marshalling to the main thread
  goes through tao's `EventLoopProxy`, which exists for this purpose and is already
  cross-platform.

### 6.3 Transport

- **Flutter** — a single binary platform channel (`duet/rpc`) through the embedder messenger.
- **Webview** — Tauri v2 IPC: `invoke` for commands, Tauri events for patches.

The codec sits behind a `Codec` trait. **v1 ships JSON**: debuggable, trivial, and the
payloads are small in-process patches. A compact binary codec is a drop-in replacement later
without touching any public API. Optimising before a benchmark exists would be guesswork.

### 6.4 Multiple Flutter windows

`FlutterSurface` exposes N windows regardless of the underlying strategy, choosing:

1. **One engine, N views** where the platform's multi-view support allows it — much lower
   marginal cost per window.
2. **N engines** otherwise, sharing snapshot and isolate group where the API permits.

App authors never observe the difference. This is the highest-churn area of the design:
Flutter desktop multi-view is actively evolving upstream, so this abstraction is partly
insulation against a moving target. Keeping it behind `FlutterSurface` means upstream churn
costs one crate rather than a public API break.

> **Spike A finding (2026-08-04): strategy 1 is unavailable on macOS today.** A second
> concurrent `initWithEngine:` throws `NSInternalInconsistencyException — The engine already
> has a view controller for the implicit view`. Both controllers report `viewIdentifier=0`,
> the *implicit view*. Sequential detach-then-create works; simultaneous does not.
>
> The header docs read as though arbitrary multi-view were supported; on this engine build it
> is not. **macOS must use one engine per Flutter window.** The abstraction held — this was
> already the named fallback — but strategy 1 should not be planned around until re-verified.
>
> This does not affect Duet's primary shape (one Flutter window plus one webview window), and
> it makes the replace-not-duplicate teardown model the natural fit for this engine.

---

## 7. Codegen

Derive macros (`#[derive(SharedState)]`, `#[command]`) register type information into an
inventory. The host binary gains a `--dump-schema` flag; `duet codegen` runs it, and feeds
the resulting `schema.json` to the Dart and TS emitters. This mirrors `tauri-specta` — no
build-script fragility and no parsing of Rust source.

| Rust | Dart | TS |
|---|---|---|
| `bool`, `i32`, `f64` | `bool`, `int`, `double` | `boolean`, `number`, `number` |
| `String` | `String` | `string` |
| `Option<T>` | `T?` | `T \| null` |
| `Vec<T>` | `List<T>` | `T[]` |
| `HashMap<String, V>` | `Map<String, V>` | `Record<string, V>` |
| `struct` | immutable class + `copyWith` | `interface` |
| unit enum | `enum` | string union |
| data enum | sealed class | discriminated union |
| `Vec<u8>` | `Uint8List` | `Uint8Array` |

Generated Dart classes are immutable and expose `copyWith`.

**Known wart:** `i64` maps to TS `number` and loses precision above 2^53. This is documented,
with `#[duet(bigint)]` available as an opt-in producing `bigint` on the TS side.

---

## 8. Tooling

### 8.1 Project layout

```
my-app/
  duet.toml
  host/      src/main.rs, src/state.rs        # Rust — source of truth
  flutter/   lib/main.dart, lib/generated/    # standard Flutter package
  web/       src/main.ts, src/generated/      # Vite app
```

### 8.2 `duet dev`

```
duet dev
 ├─ flutter assemble (debug) → kernel_blob.bin + flutter_assets
 ├─ persistent frontend_server ← incremental recompiles
 ├─ vite dev server            ← web HMR
 ├─ cargo run (host, DUET_DEV=1) → prints Dart VM service URI
 └─ watch
     .dart → recompile kernel → VM service `reloadSources`
     .rs   → rebuild + restart host (state lives in Rust, so this is a real restart)
     web/* → Vite HMR
```

The decision that determines whether this is genuine hot reload: **`frontend_server` runs as
a persistent process** rather than shelling out to `flutter assemble` per change. That is
roughly 200 ms incremental recompile versus 1–3 s rebuild.

`reloadSources` is issued directly over the VM service WebSocket, **and** the VM service URI
is printed so `flutter attach` and IDE debuggers work as well. The URI costs nothing to
expose.

> **CONFIRMED by Spike C (2026-08-04).** This works, and the persistent-`frontend_server`
> decision is vindicated. Measured edit→pixel latency — file write to the changed UI actually
> rendered, including recompile, reload, rebuild and render:
>
> | Run | n | min | median | max |
> |---|---|---|---|---|
> | Spike | 10 | 111.7 ms | 123.3 ms | 165.1 ms |
> | Independent re-run | 5 | 107.5 ms | 112.9 ms | 143.9 ms |
>
> Roughly 4× inside the 500 ms bar. Incremental recompiles were 9–22 ms — faster than the
> ~200 ms estimated above — so most of the latency is VM reload plus widget rebuild, not
> compilation.
>
> It is **genuine hot reload, not restart**: Dart heap state survived every reload
> (`_tapCount` held at 3), and `reloadSources` reported `savedLibraryCount: 752` against
> `receivedLibraryCount: 2`.
>
> Three implementation requirements for Phase 4, all learned the hard way:
>
> - **`ext.flutter.reassemble` must follow `reloadSources`.** The former loads new code; only
>   the latter rebuilds the widget tree. `flutter_tools` does both.
> - **Never set `force: true` on `reloadSources`.** It causes a fatal error inside the Dart VM's
>   own C++ runtime (`"StatelessWidget which is not loaded yet"`), because it asks the VM to
>   force-reload libraries the delta does not contain.
> - **Capturing the VM service URI currently needs an fd-1 redirect**, since the engine prints
>   it from native code. Workable but ugly — look for a cleaner route (a known
>   `--vm-service-port`, or reading it off the engine object) before shipping this in the CLI.

> **SHIPPED by Phase 5 (2026-08-06), and the fd-1 redirect is gone.** `crates/duet-dev` is the
> driver and `duet dev` is the command. Two things the note above asked for both turned out to
> be available, and neither redirects a file descriptor:
>
> - **`duet dev` runs the host as a child process**, so the engine's fd 1 is the *child's*
>   fd 1 — an ordinary pipe the parent reads and echoes through. This is not a workaround; it
>   falls out of the architecture the diagram above already describes. The VM service keeps its
>   authentication code, and a host that speaks a protocol on its own stdout is unaffected.
> - **For a host driving its own reload in-process**, the macOS embedder reads engine switches
>   from `FLUTTER_ENGINE_SWITCHES` / `FLUTTER_ENGINE_SWITCH_N`. With `vm-service-port=<n>` and
>   `disable-service-auth-codes`, the URI is known *before the engine boots*. Measured: the
>   engine announced exactly `http://127.0.0.1:45671/`. The cost is that the VM service is then
>   unauthenticated on loopback for that port, which is why the child-process route is the
>   default wherever a child exists.
>
> `write-service-info=<path>` was tried first — it would have given a known URI *with* the auth
> code. The string is in `FlutterMacOS.framework`'s binary but no file ever appeared, as a plain
> path or as a `file://` URI. It is not wired up in this embedder.
>
> Re-measured end to end against a real engine, 30 reloads across three runs: **median 43 ms**
> from `fs::write` to the change being in a rendered frame *and* readable back out of the Rust
> store — a longer path than Spike C measured, and still under half its 123.3 ms median, because
> the fixture rebuilds three widgets rather than a whole `MaterialApp`. The recompile leg is
> comparable at 6–19 ms. **The Duet store's contents survive every reload**, asserted by
> `crates/duet-backend-macos/examples/hot_reload.rs`.

### 8.3 `duet build`

AOT `flutter assemble` → web production bundle → `cargo build --release` → platform bundling
delegated to Tauri's bundler (`.dmg`, `.msi`, `.deb` / `.AppImage`).

---

## 9. Failure handling

- Every surface operation returns `Result`. Failure moves the surface to `Failed(reason)`,
  published in the store. **The host never panics because a guest died.**
- Webview renderer crashes are routine rather than exceptional. They are detected via wry
  callbacks, move the surface to `Cold`, and trigger auto-resume per policy.
- Engine start failures produce actionable messages (missing assets, bad ICU data path).
- Panics inside command bodies are caught at the task-pool boundary and converted to typed
  errors: Rust `Result<T, E>` → Dart typed exception → TS rejected promise.

---

## 10. Testing

| Layer | Approach | Target |
|---|---|---|
| `duet-core` | Unit: patch computation, prefix-matching subscriptions, lifecycle transitions, policy function | 90%+ |
| `duet-codegen` | Golden files — schema fixture → expected `.dart` / `.ts` | 90%+ |
| native crates | Per-OS integration in CI: engine start/shutdown, attach/detach | smoke |
| **soak** | N teardown/resume cycles; assert RSS returns to baseline | release gate |
| clients | Dart `test` and Vitest on mirror/patch application | 85%+ |
| E2E | Sample app: suspend → resume → assert state survived | critical paths |

80%+ overall coverage is realistic because `duet-core` is the largest single chunk and is
pure. That was the purpose of keeping it platform-free.

The soak test is a **release gate, not a nicety**. Resource reclamation is the framework's
headline claim; if repeated engine cycling leaks, that claim is false. CI must fail on it.

---

## 11. Risks

| # | Risk | Mitigation |
|---|---|---|
| 1 | Hot reload inside a custom embedder — Flutter's tooling assumes its own runner, and this is a v1 requirement | Spike first, before any framework code. Kills the project cheaply if it must die |
| 2 | Run loop integration: tao + Flutter platform thread + GTK | Spike on Linux early; it is the worst case, so it sets the design |
| 3 | Repeated engine start/shutdown leaks memory | Soak test in CI from day one |
| 4 | Flutter desktop multi-view churn upstream | Insulated behind `FlutterSurface`; degrade to one engine per window |
| 5 | `objc2` memory management on macOS | Keep the Objective-C surface small and audited |

---

## 12. Sequencing

v1 scope is large — three platforms and full hot reload. The phases below are ordered so
each is independently valuable and the highest-risk work fails earliest.

| Phase | Deliverable | Rationale |
|---|---|---|
| 0 | Spikes: hot reload, run loop, view parenting ×3 | Timeboxed. Everything downstream assumes success |
| 1 | `duet-core` — store, patches, lifecycle, policy | Pure Rust, no native code, most of the coverage |
| 2 | OS window management + webview surface + TS client | Half the system working end to end |
| 3 | Flutter surface, macOS, AOT only | Proves teardown/resume and real memory reclamation |
| 4 | Codegen + CLI + hot reload | The DX layer |
| 5 | Windows and Linux platform implementations | Fill in the traits |
| 6 | Docs, examples, publish | |

Phase 0 is not optional given the chosen combination of platform breadth and hot reload
parity.
