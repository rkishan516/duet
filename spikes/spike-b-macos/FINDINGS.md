# Spike B — Run loop coexistence (macOS)

**Overall verdict: spec §6.2's threading model is SOUND on macOS.**

`tao`'s event loop, the Flutter platform thread, and a `wry` webview coexist in one process
on one main thread, and `EventLoopProxy` reliably marshals work from a background thread onto
the main thread where it can drive **both** guests. Over a 180-second continuous run,
**709 proxy events sent, 709 received, zero lost, no deadlock.**

Two criteria could not be verified in this environment and are honestly marked as such.

| # | Criterion | Verdict |
|---|---|---|
| 1 | Simultaneous coexistence, both rendering | **yes** |
| 2 | Neither starves the other | **yes** (with a WebKit rAF caveat — see F2) |
| 3 | `EventLoopProxy` drives both sides from a background thread | **yes** |
| 4 | Sustained operation, no deadlock | **yes** — 180 s, 709/709, missed=0 |
| 5 | Input routing | **cannot verify here** — no WindowServer; synthetic only, and asymmetric (F4) |
| 6 | Linux GTK + WebKitGTK | **not attempted** — platform unavailable. Remains the largest open risk |

Environment: macOS 26.5.2 arm64, Flutter 3.47.0-0.3.pre (master), Rust 1.92,
`tao` 0.36.0, `wry` (see `Cargo.toml`), `objc2` 0.6.4 with `catch-all`.

---

## The pattern Phase 2 should be written from

A background thread (Phase 2's "core thread") sends a `UserEvent` through `EventLoopProxy`.
The main thread receives it and dispatches to both guests from the same handler:

```rust
Event::UserEvent(UserEvent::Tick(n)) => {
    // 1. Flutter: platform channel via the engine's binaryMessenger
    if let Some(channel) = &state.method_channel {
        unsafe { flutter_ping(channel, n, /* reply counters */) };
    }

    // 2. Webview: evaluate JS and read a value back
    if let Some(webview) = &state.webview {
        let js = format!("(function(){{ window.__spikeB.pings++; \
                          return {{pings: window.__spikeB.pings, raf: window.__spikeB.raf}}; }})()");
        let _ = webview.evaluate_script_with_callback(&js, move |result: String| {
            // result is a JSON string; both sides confirmed to land
        });
    }
}
```

Both directions were proven to actually land, not merely to be called: Dart replied over the
channel (`pong pingCount=701 frameTicks=27652`) and the JS readback returned mutated page state
(`{"pings":701,"lastPingN":700,...}`). 700 Flutter replies and 696 webview replies over 180 s.

---

## F1 — `EventLoopProxy` is reliable under sustained load

180 s of continuous proxy traffic, one event every ~250 ms:

```
=== Sustained run complete after 180s:
    proxy events sent=709 received=709 (missed=0) - no deadlock observed ===
```

RSS 174 MB at start, 292 MB at end. Growth is consistent with Flutter engine warm-up plus
accumulated JS state rather than a leak, but this run was not designed as a leak test.

**Impact:** spec §6.2's choice of a single cross-platform marshalling mechanism is validated on
macOS. No custom `dispatch_async` path is needed.

## F2 — The rAF stall was WebKit throttling, not starvation

**This was investigated specifically because it looked like it might invalidate the design.**

In the 180 s run, the webview's `requestAnimationFrame` counter advanced normally to 9168 at
t+62 s, crawled to 9752 by t+77 s, then **froze for the remaining 100 seconds** while Flutter's
`frameTicks` kept climbing to 27652. On its face this reads as Flutter starving the webview.

It is not. Four independent pieces of evidence:

1. **`evaluate_script` never stalled.** The `pings` counter, driven from the main thread,
   advanced from 1 to 701 throughout the freeze. A starved main thread would have stalled it.
2. **Adding a `setInterval` timer prevented the stall entirely.** A 90 s re-run with a
   `setInterval` counter alongside rAF showed rAF advancing steadily to 14159 — past the 9752
   freeze value — at a constant ~157/s with no slowdown.
3. **Flutter kept rendering** the whole time, so neither side was blocked.
4. Windows reported `occlusionState: visible=true` and `document.visibilityState: "visible"`,
   so this is not simple occlusion detection — it is WebKit's page-activity throttling. A page
   with an active timer is treated as busy; a page whose only activity is rAF, in a window that
   never becomes **key** (impossible here, no WindowServer), gets its rAF suspended.

**Conclusion: benign and environmental.** On a real display the webview window becomes key when
focused. This is ordinary browser behaviour that web content already copes with.

**Caveat worth carrying into the framework docs:** a Duet webview surface that is open but never
focused may have its rAF throttled by WebKit. That is standard web behaviour rather than a Duet
bug, but framework users animating with rAF in a background window should know.

## F3 — `Running with merged UI and platform thread. Experimental.`

Also observed in Spike A. The macOS embedder merges Flutter's UI and platform threads.

**Impact on spec §6.2: none.** The three-context model already places Flutter's platform thread
on the main thread. Nothing observed here contradicts it. Worth re-checking on Windows and Linux,
where the embedder threading model differs.

## F4 — Synthetic input reaches Flutter but NOT the webview

Synthetic `mouseDown`/`mouseUp` via `-[NSWindow sendEvent:]`:

| Target | Result |
|---|---|
| Flutter window | **reached** — Dart-side `tapCount` went 0 → 1 |
| Webview window | **not reached** — JS `clicks` stayed 0 |

Likely an event-routing difference: WKWebView's hit-testing expects events through a different
path than a direct `NSWindow sendEvent:`. Not diagnosed further — real input is untestable here
regardless, so this could not be distinguished from the environment's limitations.

**Impact:** input routing to the webview must be verified on real hardware before Phase 2
depends on it. Do not assume the webview receives input just because the Flutter side does.

## F5 — `wry`'s `evaluate_script_with_callback` double-encodes returned strings

Returning an already-stringified JSON string from evaluated JS produces a double-encoded
(quoted and escaped) result in Rust, because `wry` runs the returned value through
`NSJSONSerialization` on the native side. Return a plain JS **object** instead and let `wry`
serialize it once.

This cost real debugging time in the spike. **Phase 2's webview IPC should return objects, not
pre-stringified JSON.**

## F6 — Creation order was not significant

Both windows were created back to back on the same main thread (Flutter engine first, then the
webview) with no crash. The reverse order was not exercised, since the working order was found
first and no trouble arose; noted only so nobody assumes both orders were validated.

---

## What could not be verified here

**Criterion 5, real input.** This environment has no reachable on-screen WindowServer for
spawned processes — established independently in Spike A. Real keyboard and mouse input cannot
be generated or observed. Verifying it needs a machine with an attached display under a real
login session.

**Criterion 6, Linux.** Not attempted; macOS-only machine. Flutter's GTK embedder sharing one
GTK main loop with WebKitGTK remains the least-trodden combination in the entire design and
**the largest unretired risk in Phase 0**. It needs a Linux machine or VM with GTK3, Flutter's
Linux embedder, and webkit2gtk.
