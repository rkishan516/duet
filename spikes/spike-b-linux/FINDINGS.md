# Spike B (Linux) — Run loop coexistence

**Overall verdict: the threading model is SOUND on Linux, and the porting
plan's single largest unretired risk is retired.**

docs/10-porting.md §5.1: "Flutter's GTK embedder and WebKitGTK both want the
GTK main loop. Nobody has run them together in one process." Now somebody
has: one `tao` (GTK) main loop hosted a Flutter engine and a WebKitGTK
webview in one process, driven from a background thread through
`EventLoopProxy`, for 180 seconds — **722 proxy events sent, 722 received,
zero lost** (the trilogy: macOS 709/709, Windows 723/723, Linux 722/722).
Over the run: 720 Flutter channel replies, 715 webview eval round trips,
10,734 Dart-initiated frameMarker messages answered by the native handler,
frameTicks 10,734, webview rAF 10,716 — RSS stable around 365 MB. No special
handling was needed for the loop itself — GTK is the one main loop all three
parties already speak.

| # | Criterion | Verdict |
|---|---|---|
| 1 | Simultaneous coexistence, both rendering | **yes** |
| 2 | Neither starves the other | **yes** — and no WebKitGTK rAF stall (macOS F2's WebKit throttling did not reproduce here) |
| 3 | `EventLoopProxy` drives both sides from a background thread | **yes** |
| 4 | Sustained operation, no deadlock | **yes** — see the run numbers below |
| 5 | Input routing | **cannot verify here** — autonomous runs; the WSLg windows are on a real desktop but nobody clicked them |
| W1 | Dart→native handler over the C callback API | **yes** |
| W2 | Detach as hide, reattach as show | **yes** — full cycle verified |
| L1 | Boot with public API only, no window ever shown | **yes** — the realize-boot; see L-F1/L-F2 |
| L2 | Engine survival after implicit-view destruction | **inconclusive by construction** — see L-F5 |

Environment: Windows 11 WSL2 (Ubuntu 26.04, WSLg — `DISPLAY=:0`,
`WAYLAND_DISPLAY=wayland-0`), `GDK_BACKEND=x11` (XWayland), Flutter master
`7870373` (the exact commit the Windows port used), Rust 1.97.1, `tao`
0.36.0, `wry` 0.56 via `build_gtk`, GTK 3.24.52, WebKitGTK 2.52.3,
`FLUTTER_LINUX_RENDERER=software` with Impeller disabled (L-F4).

---

## L-F1 — There is no public way to start the engine except realizing its implicit view

`fl_engine_start` exists only in `fl_engine_private.h` and is **not
exported** from `libflutter_linux_gtk.so` (checked with `nm -D`; the library
is built with hidden visibility and only the public headers' symbols are
reachable). The only caller that matters is `FlView`'s renderer-realize
callback — and only for the engine's **implicit** view:
`fl_view_new(project)` creates engine + implicit view together;
`fl_view_new_for_engine` creates a *secondary* view whose
`FlutterEngineAddView` is rejected outright on an unstarted engine
(`kInvalidArguments: Engine handle was invalid`, measured).

So on Linux, Duet's boot **is** view creation: `fl_view_new`, pack the view
into its (invisible) window, and realize.

## L-F2 — The realize-boot: realize parents-first, map nothing

Realizing a never-shown window tree is what boots the engine headless-style,
and the order is load-bearing. `fl_engine_start` hangs off the *renderer*'s
realize signal, and the renderer sits under an event box inside the FlView;
FlView's own realize override tries to skip straight to the renderer, which
GTK refuses while the event box is unrealized — piecemeal `realize()` calls
therefore boot nothing (measured: "No engine to send to" on every send).
Reproducing GTK's own mapping order — realize the toplevel, then walk the
tree realizing parents before children — starts the engine with the window
still invisible. Proven end to end: with `DUET_SPIKE_STAY_PARKED=1` the
window is never shown and Dart still answers pings and spams its per-frame
marker channel.

## L-F3 — Attach/detach is show/hide; reparenting is FATAL to the engine

The one thing the other two platforms allowed — moving the view — is the one
thing Linux forbids. `gtk_container_remove` unrealizes the FlView, and
re-realizing the implicit view re-runs `fl_engine_start` against a live
engine, which wrecks the engine handle: every subsequent task fails
(`FlutterEngineRunTask returned kInvalidArguments` in an endless storm) and
the messenger goes dead. Measured twice, from two different reparent
orderings.

`gtk_widget_hide`/`show` on the toplevel is the survivable pair: hide unmaps
but never unrealizes. The W2 probe ran the full cycle with the macOS F1
lifecycle sends around it:

```
W2 DETACH:  lifecycle inactive+hidden sent, the Flutter window hidden (frameTicks=531)
W2 DETACH CHECK after 4s parked: frameTicks 531 -> 547 (advanced 16), pings still replying
PASS criterion W2: frameTicks resumed advancing after reattach (547 -> 771, +224)
```

So the Linux backend's shape: the FlView lives in its window from boot to
destruction; `attach_view` maps the window, `detach_view` unmaps it (with
the lifecycle sends), and `destroy_renderer` destroys view and window
together. `close_window` and `destroy_renderer` are consequently more
entangled than on the other platforms — the window cannot outlive its view's
realization, so the backend owns their pairing.

## L-F4 — WSLg rendering: software renderer + Impeller off is the working pair

Three failure modes were measured on the way to a working configuration:

1. Default (OpenGL + Impeller GLES): `Could not determine GL version` →
   `Failed to create platform view rendering surface` — WSLg's X11 offers no
   DRI3 device and the Impeller GLES probe gives up.
2. `FLUTTER_LINUX_RENDERER=software` alone: boots, serves one frame, then
   **FATAL** — `Impeller DlText cannot be drawn to a Skia canvas` (the
   framework built Impeller display lists; the software raster path is
   Skia).
3. `FLUTTER_LINUX_RENDERER=software` **plus**
   `fl_dart_project_set_enable_impeller(project, FALSE)`: everything works.

The Impeller switch is a public project setter, so the backend can own it;
the renderer choice is the embedder's own environment variable. On real
Linux hardware with working GL the default path may be fine — that
combination is untested here and recorded as such.

## L-F5 — The ownership probe was inconclusive, and the design no longer needs it

The L2 probe destroyed the FlView while this spike held its own
`g_object_ref` on the engine, then pinged the messenger — but the loop
exited before the async reply could land, so the run answers neither way.
(The destroy did surface one more fact: the embedder logs
`FlutterEngineRemoveView returned kInvalidArguments — The implicit view
cannot be removed`, confirming at the API level that the implicit view is
not a removable thing; only its whole engine is.)
Unlike on Windows — where the analogous question forced the parking-window
design — nothing here depends on the answer: the show/hide model never
destroys the view before `destroy_renderer`, which takes view, window and
engine down together. GObject reference counting makes engine-outlives-view
*plausible*, and a future secondary-view design would need it; the
playground-grade fact is simply not needed for the port and was not claimed.

## L-F6 — No WebKitGTK rAF throttling appeared

macOS F2 saw WKWebView freeze `requestAnimationFrame` permanently in a
never-key window; Windows saw nothing of the kind; WebKitGTK sides with
Windows. Over the sustained run `raf` advanced continuously (1488 at the 26s
checkpoint against setInterval 477 — ~57/s, software-composited), and the
stall detector never fired.

## L-F7 — wry must target the vbox, not the window

`build_gtk(window.gtk_window())` "works" — the webview runs, evals answer —
while never being visible: tao's `GtkApplicationWindow` already contains its
default `GtkBox`, and a `GtkBin` takes exactly one child (GTK warns and the
add is a no-op). `build_gtk(window.default_vbox())` is the correct call. A
webview that answers every probe while showing nothing is exactly the kind
of silent wrongness worth a numbered finding.

---

## What could not be verified here

**Criterion 5, real input.** Autonomous runs; the WSLg windows appear on the
Windows desktop but nobody clicked them during the measured runs.

**Real-hardware GL.** Everything above ran on WSLg's DRI3-less X11 with the
software renderer. A native Linux desktop with working GL (and the default
OpenGL renderer, Impeller on) is a different configuration with its own
risks, untested here.

**Wayland-native.** `GDK_BACKEND=x11` throughout: a hidden toplevel can hold
realized GDK resources under X11, which the realize-boot depends on; Wayland
surfaces only exist once mapped, so the boot model itself may need rework
under `GDK_BACKEND=wayland`. Deliberately not attempted.
