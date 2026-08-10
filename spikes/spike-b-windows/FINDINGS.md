# Spike B (Windows) — Run loop coexistence

**Overall verdict: the threading model is SOUND on Windows, and the porting
brief's two named risks did not materialise.**

`tao`'s Win32 event loop, the Flutter Windows engine (`flutter_windows.dll`),
and a `wry`/WebView2 webview coexist in one process on one thread, and
`EventLoopProxy` reliably marshals work from a background thread onto that
thread to drive **both** guests. Over a 180-second continuous run: **723 proxy
events sent, 723 received, zero lost, no deadlock.** WebView2's message pump
and COM apartment coexisted with the Flutter engine's own task runner with no
special handling — tao's ordinary `GetMessage`/`DispatchMessage` loop pumps
both (the header's own note: engine messages are processed "transparently
through DispatchMessage").

| # | Criterion | Verdict |
|---|---|---|
| 1 | Simultaneous coexistence, both rendering | **yes** — `evidence/*.png` via `PrintWindow` |
| 2 | Neither starves the other | **yes** — and no WebView2 rAF throttling analog of macOS F2 |
| 3 | `EventLoopProxy` drives both sides from a background thread | **yes** |
| 4 | Sustained operation, no deadlock | **yes** — 180 s, 723/723, missed=0 |
| 5 | Input routing | posted `WM_LBUTTONDOWN` **reached Flutter** (tapCount 0→1); did **not** reach WebView2; real hardware input **cannot verify here** (autonomous run, no human at the desktop) |
| W1 | Dart→native handler over the C callback API | **yes** — 25,954 messages answered |
| W2 | Detach as reparent-to-parking, engine kept alive | **yes** — full cycle verified |

Environment: Windows 11 Pro 10.0.26200 x64, Flutter 3.47.0-1.0.pre-237
(master), Rust 1.92 MSVC, `tao` 0.36.0, `wry` 0.56, WebView2 Runtime
151.0.4129.72, display session with a real compositor (unlike every macOS
measurement, which was WindowServer-less — see F3 there).

---

## W-F1 — The view controller OWNS the engine; detach must be a reparent, not a destroy

The single most consequential difference from macOS, and it contradicts the
table in docs/10-porting.md §4 (which suggested detach = "destroy the view
controller, keep the engine"):

- `flutter_windows.h` on `FlutterDesktopViewControllerCreate`: "This takes
  ownership of |engine| … `FlutterDesktopEngineDestroy` will be called
  internally when the view controller is destroyed."
- Measured: `FlutterDesktopViewControllerDestroy` dropped RSS **258,080 kB →
  78,444 kB**. That is engine shutdown, not view removal.

So on Windows:

- **`destroy_renderer` maps to `FlutterDesktopViewControllerDestroy`** (or
  `FlutterDesktopEngineDestroy` if no controller was ever created) — the
  operation that reclaims memory, exactly like macOS `shutDownEngine`.
- **`detach_view` maps to reparenting the Flutter view HWND** into a hidden
  parking window (`SetParent`), bracketed by the same `flutter/lifecycle`
  sends the macOS F1 fix uses. The controller is never destroyed on detach.

The parking window exists so that closing the *visible* window can never
destroy the Flutter view HWND out from under the live controller (a Win32
window destroys its children; macOS had no analog of this hazard because
`removeFromSuperview` detaches immediately).

## W-F2 — The full detach/park/reattach cycle works (criterion W2)

With `DUET_SPIKE_B_PROBE_DETACH=1`: at t+10s the view HWND was reparented into
a hidden parking window after sending `AppLifecycleState.inactive` then
`.hidden`; at t+18s it was reparented back, shown, and sent `.inactive` then
`.resumed`.

```
W2 DETACH:  lifecycle inactive+hidden sent, view HWND reparented into hidden parking window (frameTicks=1379 at detach)
W2 DETACH CHECK after 4s parked: frameTicks 1379 -> 1416 (advanced 37), pings still replying=59 — engine alive while parked
W2 REATTACH: view HWND reparented back, shown, lifecycle inactive+resumed sent
PASS criterion W2: frameTicks resumed advancing after reattach (1416 -> 1956, +540), engine never destroyed, no backing-store storm observed in stderr
```

- While parked+hidden the Dart scheduler nearly stopped (~9 frames/s residual
  against ~135/s attached — the tail end of frames already scheduled when
  `hidden` landed). Platform-channel pings kept replying throughout: the
  engine is fully alive while parked.
- **No `Could not create the embedder backing store` storm** — the macOS F1
  signature — appeared at any point. That run used the lifecycle sends; whether
  the storm appears *without* them on Windows is deliberately left to the
  lifecycle example (the F1 reproduction experiment), since the backend sends
  them either way.
- The Dart side independently confirmed every transition
  (`[spike_app] didChangeAppLifecycleState: …` for hidden, inactive, resumed),
  so the framework's transition table accepted the macOS ordering
  (`resumed → inactive → hidden`, `hidden → inactive → resumed`) unchanged.

## W-F3 — Headless boot and the boot/attach split hold on Windows

`FlutterDesktopEngineCreate` + `FlutterDesktopEngineRun(null)` with **no view
controller** booted the engine and ran Dart (`spike_c` frameMarker traffic
started arriving before any controller existed — 313 messages by t+3.3s).
`FlutterDesktopViewControllerCreate` then attached against the
already-running engine without complaint. Duet's boot-headless-attach-later
model is expressible on Windows with no `allowHeadlessExecution` analog
needed — it is simply how the C API composes.

## W-F4 — `FlutterDesktopEngineRun` is synchronous enough for `Readiness::Ready`

`Run` returned `true` and the very first `SendWithReply` ping (dispatched from
the same event-loop turn) was answered by Dart. `start_renderer` on Windows
can return `Readiness::Ready` exactly as macOS does, and for the same reason.

## W-F5 — Dart→native over `FlutterDesktopMessengerSetCallback` works (criterion W1)

The exact API pair the real `duet/rpc` channel needs — `SetCallback` with a C
function pointer + `user_data`, replies through
`FlutterDesktopMessengerSendResponse` on the message's single-use response
handle — served the fixture's per-frame `duet/spike_c` spam for the whole run:
**25,954 messages landed on the native handler and were each answered**, at
~144/s, while the same thread also drove pings and webview evals. The handler
was invoked inline on the platform thread (counters mutated from the handler
were read consistently from the tick handler with plain atomics; no cross-
thread reordering was ever observed).

Engine boot RSS on this machine: ~16 MB → ~168 MB after `Run`, +11 MB for the
view controller, +46 MB for the WebView2 webview; steady-state ~255 MB with
both guests live.

## W-F6 — No WebView2 analog of macOS's rAF throttling appeared

macOS F2 saw WKWebView freeze `requestAnimationFrame` permanently in a
never-key window. On Windows, over 180 s in an unfocused window, `raf` and
`setInterval` advanced in lockstep the whole run (raf=4569 vs interval=635 at
t+32s — exactly the 144 Hz / 20 Hz ratio) and the rAF-stall detector never
fired. The two-scheduler discriminator stays in the spike for honesty, but on
this machine WebView2 did not throttle an unfocused visible window.

## W-F7 — Posted synthetic input reaches Flutter but not WebView2

`PostMessageW(WM_LBUTTONDOWN/UP)` at the Flutter view HWND produced a real
Dart-side tap (`tapCount` 0→1 via the channel `query`) — better than macOS,
where synthetic `NSEvent`s never reached either guest's gesture pipeline.
The same messages posted at the WebView2 child HWND did not register a DOM
`mousedown` (`clicks` stayed 0) — WebView2's input path (its own child HWND
chain / DirectComposition hit-testing) does not consume a message posted at
the outer child. Same caveat as macOS F4: real input routing needs a human at
the machine; posted messages prove WndProc delivery, not full routing.

## W-F8 — `wry`'s double-encoding rule carries to WebView2 verbatim

`evaluate_script_with_callback` returning a **plain JS object** produced clean
single-encoded JSON in Rust on every one of the 723 eval round trips
(WebView2's `ExecuteScript` JSON-serializes the completion value, same as
`NSJSONSerialization` on macOS). macOS F5's rule — return objects, never
pre-stringified JSON — ports unchanged.

## W-F9 — In-process rasterization needs `PW_RENDERFULLCONTENT`

`PrintWindow` with flag `2` (`PW_RENDERFULLCONTENT`, absent from
win32metadata — declared by hand) captured real pixels for both the Flutter
view (Impeller/OpenGLES via ANGLE, per the engine's own startup log) and the
WebView2 window. Without that flag, DirectComposition content captures black.
This is the Windows analog of the macOS spike's `cacheDisplayInRect:`
snapshot, for the lifecycle example to reuse. The spike writes BMP (no image
codec dependency in throwaway code); the committed
`evidence/both_alive_flutter.png` / `both_alive_webview.png` are lossless
PNG conversions of the run's BMPs.

## W-F10 — The embedder sends its own lifecycle states

`[spike_app] didChangeAppLifecycleState: AppLifecycleState.inactive` arrived
at startup with no manual send: the Windows embedder has built-in lifecycle
management keyed to window activation (macOS's embedder does not do this for
embedded views, which is why F1 there was a surprise). Manual sends and the
embedder's own coexisted without tripping the framework's transition
assertions in this run, but the backend should expect interleavings — e.g. an
embedder-sent `inactive` while parked — and the lifecycle example should
watch for assertion output.

---

## What could not be verified here

**Criterion 5, real input.** This was an autonomous run with no human at the
desktop; only posted messages were exercised. Verifying real mouse/keyboard
routing (including into WebView2, where posted messages do not land) needs a
person clicking real windows.

**The F1 storm's Windows reproduction.** The spike only ran detach *with* the
lifecycle sends (the fixed configuration). Whether omitting them reproduces
the macOS retry storm on Windows — docs/10-porting.md §6 predicts "the same
shape" — is the lifecycle example's experiment to run, with the spike_app
fixture and its perpetual `Ticker`.

**Creation order beyond Flutter-then-webview.** Same caveat as macOS F6: only
the working order was exercised (engine, then webview). The reverse was not.

**Second concurrent view controller per engine.** Not attempted: the header's
ownership contract (create takes the engine; destroy kills it) makes a second
`FlutterDesktopViewControllerCreate` against an owned engine structurally
wrong, and empirically probing it risks UB rather than a clean failure. The
backend enforces one-controller-per-engine in Rust instead, exactly as the
macOS backend enforces one-view-per-engine.
