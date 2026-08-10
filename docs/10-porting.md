# 10 — Porting Duet to a new platform

Duet runs on macOS. This chapter is the brief for adding Windows or Linux.

It is written to be picked up cold. Read [01 — Overview](01-overview.md) and
[02 — Architecture](02-architecture.md) first if you have not; everything else
you need is here.

---

## 1. The good news, stated precisely

**Exactly one crate is platform-specific.** `crates/duet-backend-macos` is 2,248
lines. Every other crate — the store, the runtime, the codec, the protocol, the
supervisor, the host, the schema, codegen, the derive, the CLI, the command
registry, the dev server — is platform-free and already compiles and tests on
Linux in CI today.

That is not an accident. It is what the effects-as-data seams were for: the
supervisor returns `SurfaceAction` values instead of calling a window API, and
the host calls a `WindowBackend` trait instead of a platform. A port implements
two traits and writes nothing else.

Both guest client packages are already cross-platform: `packages/duet` is pure
Dart, `packages/duet-js` is an npm package with zero runtime dependencies, and
`packages/duet_flutter` is a `BasicMessageChannel` binding that does not care
what OS it runs on. **No guest-side work is required for a port.**

## 2. The two seams a backend implements

### `WindowBackend` — `crates/duet-host/src/backend.rs:64`

```rust
pub trait WindowBackend {
    fn start_renderer(&mut self, surface: SurfaceId) -> Result<Readiness, BackendError>;
    fn attach_view(&mut self, surface: SurfaceId) -> Result<(), BackendError>;
    fn detach_view(&mut self, surface: SurfaceId) -> Result<(), BackendError>;
    fn destroy_renderer(&mut self, surface: SurfaceId) -> Result<(), BackendError>;
}
```

Four methods. `attach`/`detach` are cheap and reversible; `destroy_renderer` is
what actually reclaims memory. Keeping those distinct is the whole point — see
[04 — Lifecycle](04-lifecycle.md).

### `Sink` — `crates/duet-runtime/src/sink.rs:38`

```rust
pub trait Sink: Send + 'static {
    fn deliver(&self, batch: Vec<Notification>) -> Result<(), SinkError>;
}
```

**`deliver` runs on the core thread and must not block.** Calling any
`StoreHandle` method from inside it returns `RuntimeError::ReentrantCall`. The
macOS implementation (`ProxySink`) posts to the windowing event loop and returns
immediately; a port does the same with its own loop.

## 3. What is already cross-platform, and what is not

| Piece | macOS uses | Windows | Linux |
|---|---|---|---|
| Windowing | `tao` 0.36 | **works** — `tao` supports Win32 | **works** — GTK |
| Webview | `wry` 0.56 | **works** — WebView2 | **works** — WebKitGTK |
| Flutter embedder | `FlutterMacOS.framework`, Objective-C | **different** — a C API | **different** — a GTK/C API |

So `tao` and `wry` carry over. **The real work is the Flutter embedder**, and it
is a different API on each platform.

## 4. The Flutter engine surface you must reproduce

`crates/duet-backend-macos/src/engine.rs` exposes exactly six operations. A port
needs the same six, and nothing more:

| Operation | macOS (Objective-C) | Windows (`flutter_windows.h`, C) |
|---|---|---|
| `boot(bundle)` | `FlutterEngine initWithPrecompiledDartBundle:` + `runWithEntrypoint:` | `FlutterDesktopEngineCreate` + `FlutterDesktopEngineRun` |
| `attach(window)` | build a view, parent it into the `NSWindow` | `FlutterDesktopViewControllerCreate`, then parent its `HWND` |
| `detach()` | remove the view, keep the engine | destroy the view controller, keep the engine |
| `shutdown()` | `shutDownEngine` | `FlutterDesktopEngineDestroy` |
| `set_lifecycle_state(s)` | `sendOnChannel:message:` on `flutter/lifecycle` | `FlutterDesktopMessengerSend` on the same channel |
| `binary_messenger()` | `engine.binaryMessenger` | `FlutterDesktopEngineGetMessenger` |

**Windows should be easier than macOS was.** The macOS backend had to construct
Objective-C blocks through `block2` to register a platform-channel handler
(`FlutterBinaryMessageHandler` and `FlutterBinaryReply` are ObjC blocks). The
Windows embedder is a plain C API with function pointers and a user-data
pointer — no `objc2`, no `block2`, no `catch-all` exception bridging.

Read `crates/duet-backend-macos/src/engine.rs` and `flutter_surface.rs` as the
reference implementation. They are heavily commented with *why*, not just what.

## 5. Order of work

Do these in order. Each is independently valuable and independently provable.

**5.1 — A windowing spike, before any Duet code.** Prove `tao` + `wry` + a
Flutter engine coexist on one thread and one event loop. This is the exact
question [Spike B](superpowers/spikes/2026-08-04-phase0-findings.md) answered for
macOS, and it is the single largest unretired risk on both remaining platforms.
Do not write a backend before this answers yes.

**On Linux this is the risk.** Flutter's GTK embedder and WebKitGTK both want
the GTK main loop. Nobody has run them together in one process. If they cannot
share it, the Linux design changes shape — possibly to separate processes — and
you want to know that before writing 2,000 lines against the assumption.

**On Windows the risk is lower but real**: WebView2 requires a message pump and
COM apartment initialisation, and the Flutter Windows engine runs its own.

**5.2 — `crates/duet-backend-windows`, mirroring the macOS crate.** Same module
split: `engine.rs`, `webview.rs`, `flutter_surface.rs`, `backend.rs`, `sink.rs`.
Mirroring it deliberately makes the two readable side by side.

**5.3 — Port the seven examples.** They are the proof, and they are the
acceptance criteria. `crates/duet-backend-macos/examples/` holds:

| Example | Proves |
|---|---|
| `webview_state.rs` | a JS guest shares the store |
| `flutter_state.rs` | a Dart guest shares the store, incl. bit-exact `f64` |
| `two_guests.rs` | both guests live at once and cannot disturb each other |
| `webview_commands.rs` / `flutter_commands.rs` | commands from each guest |
| `lifecycle.rs` | teardown reclaims memory |
| `hot_reload.rs` | reload preserves store contents |

A Windows port is done when its equivalents pass. Not before.

**5.4 — CI.** The workflow is ubuntu-only and excludes `duet-backend-macos`
because it cannot compile there. A `windows-latest` job could actually build and
test a Windows backend, which macOS never got — that would be the first
platform backend under CI.

## 6. Things that will bite you

Each of these cost real time on macOS. They are in `FINDINGS.md` with
measurements; these are the ones most likely to recur.

- **Teardown order is a correctness requirement.** Subscriptions must be dropped
  *before* the renderer is destroyed. The host already does this
  (`crates/duet-host/src/host.rs:181`, inside `perform_teardown`) — do not work
  around it in a backend.

- **Detaching a view while Dart still requests frames spins.** On macOS this
  produced 162,686 backing-store errors in 90 seconds and pinned a core. The fix
  was sending `AppLifecycleState` on `flutter/lifecycle` *before* detaching, and
  the transitions are validated (`resumed → inactive → hidden`; you cannot jump).
  Expect the same shape on Windows.

- **What reclaims memory is engine shutdown, not view detach.** Detaching gave
  back essentially nothing (223 MB before and after). Measure this on your
  platform rather than assuming the macOS ratio.

- **A guest push must be filtered by subscriber inside the surface**, not by the
  caller. Both macOS surfaces do it through one shared predicate. A caller that
  forgets must not be able to leak one guest's state to another.

- **The reentrancy guard is a same-thread check.** It cannot see a cycle through
  two threads, which is why a command handler must not block.

## 7. How to know you are done

The bar this project has held throughout: **never claim an unobserved pass.**

A backend is finished when its examples print `ALL PASS` on real hardware and
you have pasted that output. Not when it compiles, and not when it looks right.

If something cannot be verified on your machine — no display, no device — say so
explicitly, the way [09 — Limitations](09-limitations.md) does. A recorded
"cannot verify here" is worth more than a green checkmark nobody watched.

## 8. Useful commands

```bash
cargo test --workspace --exclude duet-backend-macos --locked -- --test-threads=1
cargo clippy --workspace --exclude duet-backend-macos --all-targets --locked -- -D warnings
cargo tree -p duet-core            # must print exactly one line, forever
```

Running an example needs a built Flutter bundle:

```bash
cd fixtures/duet_guest && flutter build macos --debug   # or windows
DUET_APP_FRAMEWORK_PATH=<path to the built bundle> \
  cargo run -p duet-backend-macos --example two_guests
```

**Note for a fresh clone:** `crates/duet-backend-macos/build.rs` and
`examples/showcase/host/build.rs` default the Flutter SDK location to the
original author's path. Set `FLUTTER_MACOS_FRAMEWORK_DIR` to override, or fix
the default to locate the SDK from `which flutter` — a worthwhile first commit
on any machine that is not the one this was written on.

## 9. Where the reasoning lives

Every non-obvious decision has a recorded reason. Before changing something that
looks odd, check whether it is load-bearing:

- `crates/duet-backend-macos/FINDINGS.md` — every platform measurement, numbered
  F1 through F26, including the ones that contradicted an earlier conclusion.
- `spikes/*/FINDINGS.md` — what the three Phase 0 spikes proved, and the two
  crashes they hit and fixed.
- `docs/superpowers/plans/` — the implementation plans, in order. Useful as
  history; the code and `docs/01`–`09` are the truth.
