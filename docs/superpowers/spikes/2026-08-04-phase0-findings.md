# Phase 0 — Spike Findings

Charter: `docs/superpowers/plans/2026-08-04-phase0-spike-charter.md`

| Spike | Question | Verdict |
|---|---|---|
| A | Engine-first embedding and view parenting | **YES** (macOS; Windows/Linux pending) |
| B | Run loop coexistence | **YES** (macOS; Linux NOT attempted — largest open risk) |
| C | Hot reload in a custom embedder | **YES** — genuine hot reload, ~113 ms median |

Environment: macOS 26.5.2, arm64. Flutter 3.47.0-0.3.pre (master), Dart 3.13.0.
Rust 1.92. `objc2` 0.6.4, `objc2-app-kit`/`objc2-foundation` 0.3.2, `tao` 0.36.0.

Spike code and detailed write-up: `spikes/spike-a-macos/` (`FINDINGS.md`, `src/main.rs`).

---

## Spike A — Engine-first embedding (macOS): **YES**

All five exit criteria demonstrated by a running Rust binary. Verified by the controller
re-running the spike independently, not from the report alone.

| # | Criterion | Verdict | Evidence |
|---|---|---|---|
| 1 | Engine boots with no view, process alive | yes | `runWithEntrypoint(nil)` → `true`, zero views attached, Dart VM service URI printed |
| 2 | View created, parented into a `tao` window, renders | yes | Pixel-verified PNG of the Flutter counter app, `evidence/view1_counter_app.png` |
| 3 | View detached/destroyed, engine keeps running | yes | Engine remained messageable; proven conclusively by criterion 4 reusing the same engine |
| 4 | Second view created against the same engine, renders | yes (**sequentially only** — see F2) | `controller2 viewIdentifier=0 attached=true`, rendered |
| 5 | Engine shut down cleanly, no crash on exit | yes | `shutDownEngine`, process exits 0 |

### Confirmed API sequence (macOS)

This replaces the "signatures to be confirmed" note in spec §6.1.

```
FlutterDartProject  initWithPrecompiledDartBundle:  <NSBundle of App.framework>
FlutterEngine       initWithName:project:allowHeadlessExecution:YES
FlutterEngine       runWithEntrypoint:nil                        -> BOOL
FlutterViewController initWithEngine:nibName:bundle:              (per view)
  .view -> NSView, addSubview into the tao NSWindow's contentView
detach = removeFromSuperview + drop the controller (engine holds it only weakly)
FlutterEngine       shutDownEngine
```

`allowHeadlessExecution:YES` is what permits the engine to exist with zero views.
The `engine.viewController` property must **not** be used — it is the legacy single-view
path and reassigning it throws.

---

### F1 — Assets must come from an `NSBundle`; there is no assets-path API on macOS

`FlutterDartProject` exposes only `initWithPrecompiledDartBundle:(NSBundle*)`. macOS has no
`initWithAssetsPath:`. The working approach is `NSBundle.bundleWithPath(".../App.framework")`,
where Flutter's `App.framework` carries `CFBundleIdentifier = io.flutter.flutter.app` and
`Resources/flutter_assets/`.

**Impact — spec §8 (tooling).** The `duet` CLI cannot simply copy `flutter_assets` next to the
binary. It must either produce a real `.app` bundle or lay out an `App.framework`-shaped
directory for the host to point at. This is a build-tooling requirement, not a detail.

### F2 — One engine supports only ONE Flutter view at a time on macOS

Attempting a second concurrent `initWithEngine:` while one is attached throws:

```
NSInternalInconsistencyException
reason: The engine already has a view controller for the implicit view.
  -[FlutterEngine addViewController:]
  -[FlutterViewController initWithEngine:nibName:bundle:]
```

Both controllers report `viewIdentifier=0` — the *implicit view*. Sequential
detach-then-create works; simultaneous does not. The header docs read as if arbitrary
multi-view is supported ("suitable for both the first Flutter view controller and the
following ones of the app"); on this engine build it is not.

**Impact — spec §6.4.** The "one engine, N views" strategy is unavailable on macOS today.
`FlutterSurface` must use **one engine per Flutter window**. The spec already named this as
the fallback and the highest-churn area of the design, so the abstraction holds — but the
preferred branch is currently dead on macOS and should not be planned around.

This does **not** affect Duet's primary shape (one Flutter window + one webview window),
and it makes the replace-not-duplicate teardown model the natural fit.

### F3 — Detaching a view reclaims almost no memory; only engine shutdown does

Measured RSS across the run:

| Point | RSS |
|---|---|
| Process start | 14 MB |
| After engine boot (headless, no view) | 148 MB |
| View 1 attached and rendering | 223 MB |
| View 1 detached, engine alive | 223 MB |
| After 8 detach/recreate cycles | 229 MB |
| **After `shutDownEngine`** | **104 MB** |

**Impact — spec §5.1, and it is the most consequential finding.** The `Suspending` state
(view detached, engine alive) reclaims essentially nothing — the engine, its isolate, and its
caches are the entire footprint, not the view. Only `Cold` (engine shut down) reclaims real
memory, roughly 125 MB here.

So `Suspending` is purely an **anti-thrash latency optimisation, not a memory state**. The
spec's framing is still correct but its emphasis was wrong: the grace period buys responsiveness,
not footprint. That argues for keeping the default grace short — the 5 s default should be
re-examined against measured engine boot cost, since every second of grace is a second of full
memory retention.

### F4 — `objc2`'s `catch-all` feature is load-bearing, not optional

By default an Objective-C exception crossing into Rust produces
`fatal runtime error: Rust cannot catch foreign exceptions, aborting` — no message, no reason,
no backtrace. Enabling `objc2`'s `catch-all` converts these into Rust panics carrying the
`NSException` `reason:` string, which is the only reason F2 was diagnosable at all.

**Impact — spec §9 (failure handling).** Without it, any ObjC-side assertion — misuse, a macOS
version quirk, an engine bug — is a silent undebuggable abort of the whole host. `duet-flutter`
must enable it.

### F5 — Detach → recreate must cross an event-loop tick

Detaching and immediately recreating inside the same synchronous callback races into the F2
exception even though the old controller was already dropped. Deallocation and the engine's
internal deregistration complete on a later run-loop turn.

**Impact — spec §5.1.** The `Suspending → Cold → Starting` path cannot be driven synchronously;
the state machine must yield to the run loop between teardown and re-create. `duet-core`'s
lifecycle is already event-driven, so this is a constraint on the Phase 3 host loop rather than
a design change.

### F6 — macOS merges the UI and platform threads

The engine logs `Running with merged UI and platform thread. Experimental.` on startup.

**Impact — spec §6.2.** The three-context threading model (main/UI, core, task pool) is
unaffected, since it already places Flutter's platform thread on the main thread. Worth
re-checking in Spike B, because merged threads change what can block what.

### F7 — The Dart VM service is already exposed — good news for Spike C

The headless engine printed `The Dart VM service is listening on http://127.0.0.1:53146/…`
with no special configuration. Spike C's criterion 2 (a VM service URI that `flutter attach`
can connect to) may be substantially easier than the charter assumed.

### F8 — Open: rapid view cycling logs backing-store errors

Each of the 8 detach/recreate cycles logged:

```
[ERROR:flutter/shell/platform/embedder/embedder.cc] Could not create the embedder backing store.
[ERROR:flutter/lib/ui/window/platform_configuration.cc] Reported frame time is older than the last one; clamping.
```

Rendering recovered each time and no crash occurred, but this is unexplained. It may be an
artefact of cycling faster than any real user would. **Flag for the Phase 3 soak test**, which
is already a release gate for exactly this class of problem.

### Inconclusive — per-cycle leak

8 detach/recreate cycles grew RSS by roughly 300–500 KB each. Too few cycles to distinguish a
genuine leak from a cache warming up and plateauing. Deliberately **not** called either way.
The Phase 3 soak test (hundreds of cycles, with `leaks`/Instruments) is the right instrument.

---

## Spike B — Run loop coexistence (macOS): **YES**

**Spec §6.2's threading model is sound on macOS.** Full write-up: `spikes/spike-b-macos/FINDINGS.md`.

| # | Criterion | Verdict |
|---|---|---|
| 1 | Simultaneous coexistence, both rendering | yes |
| 2 | Neither starves the other | yes (WebKit rAF caveat, B2) |
| 3 | `EventLoopProxy` drives both sides from a background thread | yes |
| 4 | Sustained, no deadlock | yes — 180 s, 709 sent / 709 received, missed=0 |
| 5 | Input routing | **cannot verify here** (B4) |
| 6 | Linux GTK + WebKitGTK | **not attempted** — largest open risk |

### B1 — `EventLoopProxy` is reliable; the core-thread model is validated

180 s of continuous proxy traffic driving both guests from one `UserEvent` handler:
709 events sent, 709 received, zero lost, no deadlock, 700 Flutter channel replies and 696
webview eval round-trips. Both directions proven to land — Dart replied over the channel and
the JS readback returned mutated page state.

**Impact — spec §6.2.** The single cross-platform marshalling mechanism is validated on macOS;
no per-platform `dispatch_async`/`PostMessage`/`g_idle_add` path is needed there.

### B2 — An apparent starvation turned out to be WebKit rAF throttling

Investigated specifically because it looked like it might invalidate the design. The webview's
`requestAnimationFrame` froze at t+77 s while Flutter kept rendering for another 100 s.

It is **not** starvation. `evaluate_script` calls kept landing throughout (`pings` 1 → 701 during
the freeze), Flutter kept rendering, and a re-run that added a `setInterval` timer showed rAF
advancing steadily past the freeze value with no slowdown at all. Windows reported
`occlusionState: visible` and `visibilityState: "visible"`, so this is WebKit's page-activity
throttling of rAF in a window that never becomes **key** — impossible to avoid here, since there
is no WindowServer.

**Impact:** benign and environmental. Worth one line in the framework docs: a webview surface
that is open but never focused may have rAF throttled by WebKit. Standard web behaviour, not a
Duet bug.

### B3 — Synthetic input reaches Flutter but not the webview

Synthetic `mouseDown`/`mouseUp` via `-[NSWindow sendEvent:]` incremented Dart's `tapCount`
0 → 1, but the webview's JS `clicks` stayed 0. Probably a WKWebView hit-testing difference,
not diagnosed further because real input is untestable here anyway.

**Impact:** do not assume the webview receives input just because Flutter does. Verify input
routing on real hardware before Phase 2 depends on it.

### B4 — `wry` double-encodes returned strings

Returning an already-stringified JSON string from evaluated JS yields a double-encoded result,
because `wry` runs the return value through `NSJSONSerialization`. Return a plain JS **object**
and let `wry` serialize once. **Phase 2's webview IPC should return objects.**

### B5 — Merged UI and platform thread, again

`Running with merged UI and platform thread. Experimental.` appears here as in Spike A. No
observed conflict with spec §6.2, which already places Flutter's platform thread on the main
thread. Re-check on Windows and Linux, where embedder threading differs.

---

## Spike C — Hot reload in a custom embedder (macOS): **YES**

**The highest-risk assumption in the project holds.** A Rust-hosted Flutter engine in JIT mode
hot-reloads via the Dart VM service, driven by a `frontend_server` we manage ourselves, with
genuine hot-reload semantics and latency roughly 4× inside the 500 ms bar.

Full write-up: `spikes/spike-c-macos/FINDINGS.md`. Verified by the controller re-running it.

| # | Criterion | Verdict |
|---|---|---|
| 1 | JIT kernel loads and renders in a Rust host | yes (reused Spike A) |
| 2 | VM service URI printed and usable | yes |
| 3 | Persistent `frontend_server` emits incremental diffs | yes — 9–22 ms per recompile |
| 4 | `reloadSources` applies the diff, **Dart heap state survives** | **yes** |
| 5 | Edit→pixel latency under 500 ms | **yes — median ~113 ms** |

### C1 — Latency is roughly 4× inside the requirement

Measured from *source file written* to *the new marker observed over a platform channel from a
post-frame callback* — so the interval includes recompile, `reloadSources`, widget rebuild and
render, not merely the RPC round trip.

| Run | n | min | median | max |
|---|---|---|---|---|
| Subagent | 10 | 111.7 ms | 123.3 ms | 165.1 ms |
| **Controller re-run** | 5 | **107.5 ms** | **112.9 ms** | **143.9 ms** |

**Impact — spec §8.2.** The hot-reload-parity requirement that drove v1's scope is achievable.
The persistent-`frontend_server` decision is vindicated: incremental recompiles are 9–22 ms, so
the bulk of the 113 ms is VM reload plus rebuild, not compilation.

### C2 — It is genuine hot reload, not hot restart

Two independent proofs:

1. **Dart heap state survived.** `_tapCount` was primed to 3 and stayed 3 across every reload in
   three separate runs. A restart resets it to 0.
2. **The reload was incremental**, per `reloadSources`' own report:

```json
{"receivedLibraryCount":2, "savedLibraryCount":752, "finalLibraryCount":753,
 "receivedLibrariesBytes":22912, "success":true, "type":"ReloadReport"}
```

752 libraries **saved**, only 2 received (22 KB). A restart would reload all 753.

### C3 — `ext.flutter.reassemble` is required after `reloadSources`

`reloadSources` alone loads the new code but does not rebuild the widget tree. The host must
follow it with an `ext.flutter.reassemble` service-extension call, exactly as `flutter_tools`
does. Both calls appear in every measured iteration.

### C4 — A stray `force: true` on `reloadSources` causes a VM-fatal crash

Adding `"force": true` to the `reloadSources` params produced a fatal error inside the Dart VM's
own C++ runtime (`"StatelessWidget which is not loaded yet"`) — it asks the VM to force-reload
libraries the delta does not contain. Real `flutter_tools` never sets it.

Worth recording because the failure is opaque and fatal, and because the subagent pursued two
plausible-but-wrong fixes (splicing the baseline kernel over the bundle's `kernel_blob.bin`;
switching to `package:` URIs) before isolating the real cause, then verified by reverting both.
**Phase 4's CLI must not set `force`.**

### C5 — Capturing the VM service URI needs an fd-1 redirect

The engine prints the URI to stdout from native code, so Rust cannot see it through normal
means. The spike redirects fd 1 to capture it (`src/stdout_capture.rs`). Workable, but ugly.

**Phase 4 should look for a cleaner route** — the VM service URI may be obtainable from the
engine object or via `--vm-service-port` with a known port — before shipping an fd-redirect in
the CLI.

---

## Phase 0 verdict: all three spikes PASS. The architecture is sound.

The design rests on three assumptions and all three now have running-code evidence on macOS.
Nothing found in Phase 0 invalidates the architecture; the corrections were to detail
(§6.1 signatures, §6.4 multi-view, §5.1 memory semantics, §8 asset bundling).

## Open risks carried into Phase 1+

1. **Linux (Flutter GTK + WebKitGTK in one GTK main loop)** — not attempted, no platform
   available. The least-trodden combination in the design and now **the largest unretired risk
   in the project**. Spec §12 Phase 5 should front-load it rather than treat it as fill-in.
2. **Windows** — engine-first signatures and run loop both unverified.
3. **Real input routing**, especially to the webview — untestable here; synthetic input reached
   Flutter but not the webview.
4. **Engine cycling leaks** — Spike A's 8-cycle sample was inconclusive; the Phase 3 soak test
   is the right instrument.
5. **`Could not create the embedder backing store`** under rapid view cycling — unexplained,
   flagged for the soak test.
