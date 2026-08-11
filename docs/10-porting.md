# 10 — Porting Duet to a new platform

Duet runs on macOS, Windows **and Linux**. This chapter was the brief for
adding Windows or Linux; both ports have since been completed by following
it, so it now serves as the brief for any *future* platform and as a record
of where each port confirmed or contradicted the plan — those notes are
inline, marked **[Windows: …]** and **[Linux: …]**.

The Windows port: `crates/duet-backend-windows` (same module split as the
macOS crate, written to be read side by side with it),
`spikes/spike-b-windows/FINDINGS.md` (the windowing spike, W-F1…W-F10), and
`crates/duet-backend-windows/FINDINGS.md` (the recorded evidence: all seven
examples observed passing on real hardware, WB1…WB6).

The Linux port: `crates/duet-backend-linux` (same module split again),
`spikes/spike-b-linux/FINDINGS.md` (the windowing spike, L-F1…L-F7), and
`crates/duet-backend-linux/FINDINGS.md` (the recorded evidence: all seven
examples observed passing under WSLg, LB1…LB5 — including the two findings
only a live run could surface).

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
| `detach()` | remove the view, keep the engine | reparent the view HWND into a hidden parking window, keep the controller |
| `shutdown()` | `shutDownEngine` | `FlutterDesktopViewControllerDestroy` (owns the engine) |
| `set_lifecycle_state(s)` | `sendOnChannel:message:` on `flutter/lifecycle` | `FlutterDesktopMessengerSend` on the same channel |
| `binary_messenger()` | `engine.binaryMessenger` | `FlutterDesktopEngineGetMessenger` |

**[Windows: the original table's detach row — "destroy the view controller,
keep the engine" — turned out to be impossible.** The header is explicit once
read closely, and the spike measured it (W-F1): `FlutterDesktopViewController`
**owns** its engine, and destroying it IS engine shutdown (RSS 258 → 78 MB).
So on Windows, detach reparents the Flutter view's HWND into a hidden parking
window with the F1 lifecycle sends around it — cheap and reversible, verified
live (W-F2) — and `destroy_renderer` maps to the controller destroy. A Linux
port should check the GTK embedder's ownership contract with the same
suspicion before trusting this chapter's table.]

**[Linux: that suspicion was earned twice over.** There is no public
`fl_engine_start` at all — the engine starts inside the *implicit* view's
realize, so `boot` IS `fl_view_new` packed into a window and realized
parents-first (L-F1/L-F2) — and the realized view can never move or
re-realize without wrecking the engine (L-F3), so attach/detach is
`gtk_widget_show`/`hide` of the window the view was born in, and a window
closed under a live renderer is *deferred* until the destroy. Shutdown
contradicted the design note a second time, and only a measurement caught
it: dropping the last *owned* reference never runs `FlEngine`'s dispose —
one embedder-internal reference outlives even the view's destruction, and
the first lifecycle run reclaimed 5.4% — so `destroy_renderer` **forces**
the dispose with `g_object_run_dispose`, after which reclaim measured 56.5%
(LB2). `set_lifecycle_state` and `binary_messenger` map to
`fl_binary_messenger_send_on_channel` / `fl_engine_get_binary_messenger`.]

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

**[Windows: retired.** `spikes/spike-b-windows` answered yes on real hardware:
one tao loop pumped both — 723/723 proxy events over 180 s, zero lost (macOS
Spike B: 709/709) — with no COM or pump special-handling at all; the engine's
tasks process "transparently through DispatchMessage", exactly as its header
promises. The spike also answered the questions this section could not:
headless boot needs no `allowHeadlessExecution` analog (W-F3),
`FlutterDesktopEngineRun` is synchronous enough for `Readiness::Ready`
(W-F4), and the Dart→native callback API works at sustained load (W-F5).]

**[Linux: retired — the risk was real but the answer was still yes.** The GTK
embedder and WebKitGTK genuinely share the one GTK main loop:
`spikes/spike-b-linux` ran both under a single tao loop for 180 s at 722/722
proxy events, zero lost, completing the trilogy. What the section could not
predict: under WSLg there is no usable GL, so runs need
`FLUTTER_LINUX_RENDERER=software` with Impeller disabled (L-F4), and `wry`
must build into the tao window's default vbox, not the window (L-F7). One
coexistence hazard did survive into the crate phase — tao's GTK loop parks a
pending `WaitUntil` with no timer source armed, stalling turns 10–16 s on a
quiet session until a proxy send wakes it (LB1) — which the spike's own
constant frame traffic masked.]

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

**[Windows: all seven pass, observed on real hardware — outputs pasted in
`crates/duet-backend-windows/FINDINGS.md`. Highlights: the five `f64` probes
round-tripped bit-exactly over the Windows channel; teardown reclaimed 73.6%
of the engine's 210 MB against the same 50% floor macOS set; hot reload ran
10/10 incremental reloads at 57 ms median edit-to-rendered-frame. The
hot_reload port also flushed out three real Windows bugs in `duet-dev`
(FINDINGS.md WB4) and one linking subtlety in the engine DLL's static CRT
(WB3) that no compile check could have found.]**

**[Linux: all seven pass under WSLg — outputs pasted in
`crates/duet-backend-linux/FINDINGS.md`. Highlights: the five `f64` probes
bit-exact again; the two-guest isolation checks 12/12 with both guests up
within 1.26 s; teardown reclaimed 56.5% against the same 50% floor; hot
reload 10/10 incremental at 60.2 ms median, with no delay-load analog needed
(glibc shares one `environ`, WB3's hazard does not exist). The runs earned
their keep exactly as §7 promises: two crate-level defects no compile check
could have found — the tao `WaitUntil` stall (LB1) and
unref-never-disposes (LB2) — were caught only because the examples measure
instead of assuming.]**

**5.4 — CI.** The workflow is ubuntu-only and excludes `duet-backend-macos`
because it cannot compile there. A `windows-latest` job could actually build and
test a Windows backend, which macOS never got — that would be the first
platform backend under CI.

**[Windows: done — the `windows` job in `.github/workflows/duet.yml` builds
the crate against the real engine artifacts (`flutter precache --windows`),
holds it to the workspace clippy/doc bar, runs its unit tests — including the
closed-loop `ProxySink` test macOS must `#[ignore]` — compiles all seven
examples, and runs `duet-dev`/`duet-cli`'s suites on Windows paths, which is
the only place those paths are exercised. Running the examples themselves
still needs real hardware and stays a recorded-evidence affair.]**

**[Linux: done — the `linux` job installs the GTK/WebKitGTK headers,
populates the engine artifacts (`flutter precache --linux`), builds the
crate against the real `libflutter_linux_gtk.so`, runs its unit tests under
`xvfb` (building a tao event loop initializes GTK, which wants a display
even off-screen), and compiles all seven examples plus the showcase and
playground binaries — which moved out of the ubuntu `rust` job with it,
since the showcase now links this backend on Linux targets.]**

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
  **[Windows: the fix was carried over as designed and no storm ever appeared —
  the framework accepted the same transitions verbatim (W-F2), and the Windows
  embedder additionally sends lifecycle states of its own on window activation
  (W-F10), which coexisted with the manual sends. Whether the storm reproduces
  *without* the sends was deliberately not tested; the backend sends them
  either way.]**
  **[Linux: same story, third platform — the sends carried over verbatim
  around detach's `gtk_widget_hide`, and no storm appeared anywhere,
  including the lifecycle run that drives the exact configuration that hung
  macOS (LB3).]**

- **What reclaims memory is engine shutdown, not view detach.** Detaching gave
  back essentially nothing (223 MB before and after). Measure this on your
  platform rather than assuming the macOS ratio.
  **[Windows: measured — teardown reclaimed 73.6% of the engine's cost against
  the macOS 60–68%, and detach-as-parking reclaimed −2.3%, i.e. nothing. The
  macOS thresholds (≥50% reclaim, ≤20% by-detach) hold unchanged (WB2).]**
  **[Linux: measured, and the measuring is what made it true — the first run
  reclaimed 5.4%, because dropping the last owned reference never runs
  `FlEngine`'s dispose (an embedder-internal reference outlives the view);
  `destroy_renderer` now forces the dispose with `g_object_run_dispose`,
  after which teardown reclaims 56.5% and detach-as-hide 1.2%. The same
  thresholds hold unchanged (LB2/LB3).]**

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
cargo test --workspace --exclude duet-backend-macos --exclude duet-backend-windows --exclude duet-backend-linux --exclude duet-showcase --locked -- --test-threads=1
cargo clippy --workspace --exclude duet-backend-macos --exclude duet-backend-windows --exclude duet-backend-linux --exclude duet-showcase --all-targets --locked -- -D warnings
cargo tree -p duet-core            # must print exactly one line, forever
```

(The excludes exist for the platforms you are *not* on — plus the showcase,
which links whichever backend matches the current OS; on a Windows machine
`cargo test -p duet-backend-windows` runs fine, on a Linux one with the GTK
headers and a Flutter SDK `cargo test -p duet-backend-linux -p duet-showcase`
does — verified on both port machines.)

Running an example needs a built Flutter bundle. On macOS:

```bash
cd fixtures/duet_guest && flutter build macos --debug
DUET_APP_FRAMEWORK_PATH=<path to the built App.framework> \
  cargo run -p duet-backend-macos --example two_guests
```

On Windows (the scaffolding is regenerated on a fresh clone —
`flutter create --platforms=windows --org com.example --project-name duet_guest fixtures/duet_guest`
first if `fixtures/duet_guest/windows/` is missing):

```bash
cd fixtures/duet_guest && flutter build windows --debug
# DUET_FLUTTER_BUNDLE defaults to the path below; set it only when running
# from another directory.
DUET_FLUTTER_BUNDLE=fixtures/duet_guest/build/windows/x64/runner/Debug/data \
  cargo run -p duet-backend-windows --example two_guests
```

On Linux (scaffold with `--platforms=linux` first, same as above; under WSLg
the two env vars are required — no usable GL there — while GL-capable
desktops may drop them):

```bash
cd fixtures/duet_guest && flutter build linux --debug
FLUTTER_LINUX_RENDERER=software GDK_BACKEND=x11 \
  cargo run -p duet-backend-linux --example two_guests
```

**Note for a fresh clone:** `crates/duet-backend-macos/build.rs` defaults the
Flutter SDK location to the original author's path. Set
`FLUTTER_MACOS_FRAMEWORK_DIR` to override, or fix the default to locate the
SDK from `which flutter` — a worthwhile first commit on any machine that is
not the one this was written on. `crates/duet-backend-windows/build.rs` and
`crates/duet-backend-linux/build.rs` start out with that fix: they discover
the SDK from the `flutter` on PATH, with `FLUTTER_WINDOWS_ENGINE_DIR` /
`FLUTTER_LINUX_ENGINE_DIR` as overrides. (`examples/showcase/host/build.rs`
is something else now: the Linux rpath helper that lets the showcase's
binaries find `libflutter_linux_gtk.so` at runtime.)

## 9. Where the reasoning lives

Every non-obvious decision has a recorded reason. Before changing something that
looks odd, check whether it is load-bearing:

- `crates/duet-backend-macos/FINDINGS.md` — every platform measurement, numbered
  F1 through F26, including the ones that contradicted an earlier conclusion.
- `crates/duet-backend-windows/FINDINGS.md` — the Windows port's measurements
  and recorded example passes, numbered WB1 through WB6, including the two
  places the platform contradicted this chapter's plan.
- `crates/duet-backend-linux/FINDINGS.md` — the Linux port's measurements and
  recorded example passes, numbered LB1 through LB5, including the two
  defects (a tao event-loop stall, an engine that never disposed) that only
  running the examples could catch.
- `spikes/*/FINDINGS.md` — what the Phase 0 spikes proved, and the crashes
  they hit and fixed; `spikes/spike-b-windows/FINDINGS.md` (W-F1…W-F10) and
  `spikes/spike-b-linux/FINDINGS.md` (L-F1…L-F7) are the run-loop spikes the
  two ports were designed from.
- `docs/superpowers/plans/` — the implementation plans, in order. Useful as
  history; the code and `docs/01`–`09` are the truth.
