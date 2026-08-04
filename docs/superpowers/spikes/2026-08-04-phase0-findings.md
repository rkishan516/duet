# Phase 0 — Spike Findings

Charter: `docs/superpowers/plans/2026-08-04-phase0-spike-charter.md`

| Spike | Question | Verdict |
|---|---|---|
| A | Engine-first embedding and view parenting | **YES** (macOS; Windows/Linux pending) |
| B | Run loop coexistence | not started |
| C | Hot reload in a custom embedder | not started |

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

## Spike B — Run loop coexistence: not started

## Spike C — Hot reload in a custom embedder: not started

F7 is an encouraging early signal: the VM service is exposed by default.
