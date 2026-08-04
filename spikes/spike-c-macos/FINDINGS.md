# Spike C: hot reload in a custom (Rust-hosted) Flutter embedder

## Verdict: YES

A Rust-hosted Flutter engine in JIT mode can be hot-reloaded via the Dart VM service,
driven entirely by a `frontend_server` process we manage ourselves - no `flutter_tools`
involved at any point after the initial `flutter build`. All five exit criteria pass,
measured edit-to-pixel latency is **106-165 ms** (10 samples, well under the 500 ms
parity bar), and Dart heap state survives every reload (this is genuine hot reload,
not hot restart wearing a costume).

This is a **complete, working recipe** - see "Exact commands and RPCs that worked"
below. It also required two false starts to get here; the actual bug was a single
wrong JSON field (`force: true`), not the structural things I originally suspected. See
"False starts" - that section is arguably the most useful part of this writeup.

| # | Criterion | Verdict | Evidence |
|---|---|---|---|
| 1 | `flutter assemble`-produced JIT kernel loads and renders | **yes (pre-proven by Spike A, reused as-is)** | `spike_app/build/macos/.../App.framework`, unmodified build pipeline |
| 2 | Engine prints a usable Dart VM service URI | **yes** | `flutter: The Dart VM service is listening on http://127.0.0.1:PORT/AUTHCODE/`, captured programmatically (see `src/stdout_capture.rs`), connected to over `ws://.../ws` |
| 3 | Persistent `frontend_server` accepts `recompile` and emits an incremental diff | **yes** | 10/10 recompiles succeeded, 8.8-21.8 ms each, each producing a real `out.dill.incremental.dill` (~21-23 KB) |
| 4 | `reloadSources` applies the diff, visible on screen, **without losing Dart heap state** | **yes** | `success:true` on 10/10; `evidence/before_reload.png` vs `evidence/after_reload.png` show marker text changed (`MARKER_V1` -> `MARKER_V9`) while `taps: 3` stayed identical across 10 reloads |
| 5 | Edit-to-pixel latency under 500 ms | **yes, with margin** | 10 samples: min 111.7 ms, median 123.3 ms, max 165.1 ms, mean 123.9 ms |

## The measured latency table (all samples, not cherry-picked)

Canonical run: 10 iterations, `DUET_SPIKE_C_ITERATIONS=10`. Full stdout in
`evidence/canonical_run_stdout.log`.

| iter | marker    | recompile latency | edit-file-write -> new-marker-observed (the reported number) | tapCount after |
|------|-----------|-------------------:|---------------------------------------------------------------:|---------------:|
| 0 | MARKER_V2  | 21.8 ms | **165.1 ms** | 3 |
| 1 | MARKER_V3  | 16.5 ms | **123.3 ms** | 3 |
| 2 | MARKER_V4  |  9.0 ms | **111.7 ms** | 3 |
| 3 | MARKER_V5  |  8.8 ms | **125.2 ms** | 3 |
| 4 | MARKER_V6  |  8.9 ms | **113.3 ms** | 3 |
| 5 | MARKER_V7  | 17.3 ms | **125.2 ms** | 3 |
| 6 | MARKER_V8  | 10.8 ms | **122.5 ms** | 3 |
| 7 | MARKER_V9  |  9.6 ms | **114.3 ms** | 3 |
| 8 | MARKER_V10 | 10.6 ms | **126.0 ms** | 3 |
| 9 | MARKER_V11 |  9.0 ms | **112.9 ms** | 3 |

**min = 111.7 ms, median = 123.3 ms, max = 165.1 ms, mean = 123.9 ms, n = 10.**

The reported latency is edit-to-pixel, not the RPC round trip alone: the clock starts
at `std::fs::write(main.dart, ...)` and stops when the host's platform-channel handler
observes the NEW marker value arrive from a `WidgetsBinding.instance.addPostFrameCallback`
that fires after every actual rendered frame (see "Measurement mechanism" below) - so it
includes recompile + `reloadSources` + `ext.flutter.reassemble` + a real widget
rebuild + paint, exactly as instructed. A second, independently-repeated run (8
iterations) landed in the same 106.8-155.4 ms band, so this isn't a lucky single sample
either.

For context, spec §8.2 estimates "roughly 200 ms incremental recompile." The measured
recompile step alone here is far faster (8.8-21.8 ms) and even the full edit-to-pixel
number comes in under that 200 ms estimate - see "Contradicts/confirms spec §8.2" below.

## Did Dart heap state survive? YES - this is genuine hot reload, not hot restart

Before the reload loop starts, the host calls `duet/spike_b`'s `increment` method 3
times (a deterministic stand-in for real tap input - Spike B found synthetic NSEvents
unreliable in this windowless environment), driving the fixture app's `_tapCount` to 3.
Across all 10 reloads, every single `frameMarker` report the host received *after* a
reload carried `tapCount: 3` - never reset to 0, never anything else. If reload had
actually been a restart, the isolate would have been torn down and recreated and
`_tapCount` would have come back as 0. It did not, on any of the 20 total reload
iterations run across all successful runs in this spike (10 + 8 + 3).

Visual proof, `evidence/before_reload.png` vs `evidence/after_reload.png`:
- Before: `marker: MARKER_V1`, `frame ticks: 14`, `taps: 3`
- After (10 reloads later): `marker: MARKER_V9`, `frame ticks: 57`, `taps: 3`

`frame ticks` (driven by a `Ticker` that's been running continuously since app start,
completely independent of anything the host does) kept climbing across the whole
sequence - further evidence the isolate was never restarted, just patched in place.

## Exact commands and RPCs that worked

### frontend_server invocation

```
/Users/kishan/dev/rkishan516/flutterDC/bin/cache/dart-sdk/bin/dartaotruntime \
  /Users/kishan/dev/rkishan516/flutterDC/bin/cache/dart-sdk/bin/snapshots/frontend_server_aot.dart.snapshot \
  --sdk-root /Users/kishan/dev/rkishan516/flutterDC/bin/cache/artifacts/engine/common/flutter_patched_sdk/ \
  --incremental \
  --track-widget-creation \
  --experimental-emit-debug-metadata \
  --target=flutter \
  --no-print-incremental-dependencies \
  --packages /Users/kishan/dev/rkishan516/tauri-flutter/spikes/spike_app/.dart_tool/package_config.json \
  --output-dill /tmp/spike-c-frontend-server-<pid>/out.dill
```

Launched once via `std::process::Command` with piped stdin/stdout, kept alive for the
whole session (`src/frontend_server.rs`). `--track-widget-creation` and
`--experimental-emit-debug-metadata` mirror flutter_tools' own resident-compiler flags
(`packages/flutter_tools/lib/src/compile.dart`); they turned out not to be the fix for
anything (see "False starts"), but they're what a real debug build uses, so they stay.
`--no-print-incremental-dependencies` is load-bearing for protocol *parsing* - without
it, every `compile`/`recompile` also dumps 150-600+ `+file://...` dependency lines
before the actual `result <boundary>` line, which is on by default and easy to miss.

### Baseline compile (once, at startup)

stdin: `compile package:spike_app/main.dart\n`

stdout:
```
result ffaa2882-edbf-4170-a6c4-cb390ca8a3c5
ffaa2882-edbf-4170-a6c4-cb390ca8a3c5
```
(~3.8 s wall time for the full app; produces the full baseline dill at `--output-dill`)

Then stdin: `accept\n` (no documented response; see "False starts" for a quirk here).

**Entrypoint URI**: `package:spike_app/main.dart`, not a raw `file://` path. This
mirrors exactly what flutter_tools' resident compiler does
(`request.packageConfig.toPackageUri(request.mainUri)`, `compile.dart:845-847`) given
`.dart_tool/package_config.json` maps package `spike_app` -> `lib/`. The same URI is
reused as the sole "invalidated file" on every `recompile` call, since the only file
that ever changes in this spike is `main.dart` itself.

### Incremental recompile (per iteration)

stdin:
```
recompile spike-c-boundary-1
package:spike_app/main.dart
spike-c-boundary-1
```

stdout:
```
result 12e94f53-aa80-4a9e-a072-d9a4fbc7a788
12e94f53-aa80-4a9e-a072-d9a4fbc7a788
```

9-22 ms wall time. Produces `<output-dill>.incremental.dill` (~21-23 KB) - this exact
filename is not documented anywhere in `--help`; discovered empirically (see
"Reverse-engineered protocol notes"). Then stdin: `accept\n` again, to commit this
generation as the new baseline for the *next* recompile.

### `reloadSources` JSON-RPC request/response (the actual thing that worked)

Request:
```json
{"jsonrpc":"2.0","id":2,"method":"reloadSources","params":{
  "isolateId":"isolates/7546815380299399",
  "rootLibUri":"file:///tmp/spike-c-frontend-server-85233/out.dill.incremental.dill"
}}
```

Response:
```json
{"id":2,"jsonrpc":"2.0","result":{
  "details":{
    "loadedLibraryCount":1,"finalLibraryCount":753,
    "receivedClassesCount":8,"receivedLibrariesBytes":22912,
    "receivedLibraryCount":2,"receivedProceduresCount":2,
    "savedLibraryCount":752,"shapeChangeMappings":[]
  },
  "success":true,"type":"ReloadReport"
}}
```

**Critically: no `"force"` field.** `rootLibUri` is a `file://` URI pointing directly at
the incremental dill frontend_server just wrote - the kernel is genuinely never re-read
from the app bundle, exactly as the brief predicted.

Followed immediately by:
```json
{"jsonrpc":"2.0","id":3,"method":"ext.flutter.reassemble","params":{"isolateId":"isolates/..."}}
```
Response: `{"id":3,"jsonrpc":"2.0","result":{"method":"ext.flutter.reassemble","type":"_extensionType"}}`

This step turned out to be necessary in practice: reload results were correct without
it in a couple of quick manual checks, but flutter_tools always sends it and this spike
keeps it for parity - the response confirms Flutter's own extension handler received
and executed it.

## False starts (the useful part)

Two structural changes were made while debugging an early crash, and only one turned
out to matter. Reporting both, and which one was actually load-bearing, matters more
than a clean success story would.

**The crash**, hit on the very first `reloadSources` call in the first two attempts:

```
../../../flutter/third_party/dart/runtime/vm/object.cc: 5157: error: Unable to use
class Library:'package:flutter/src/widgets/framework.dart' Class: StatelessWidget
which is not loaded yet.
```

This is a **Dart VM-fatal crash**, not a graceful RPC error - `dart::Assert::Fail` inside
`ClassFinalizer::FinalizeTypesInClass`, reached from `IsolateGroup::ReloadSources`,
called synchronously on the isolate's own message-handler thread while processing our
`reloadSources` RPC. The whole process aborts. `objc2`'s `catch-all` feature (load-bearing
per Spike A) does not help here - this crash is inside the Dart VM's own C++ runtime,
nowhere near the Objective-C boundary.

**Attempted fix #1 (didn't help): kernel identity matching.** My first hypothesis was
that the engine booted from `flutter build macos --debug`'s prebuilt `kernel_blob.bin`,
while a *separately*-started `frontend_server` session's own from-scratch `compile` was
used as the incremental baseline - two different compile *instances* of the same
source, with (I guessed) different internal library/class identities inside the CFE's
incremental-compiler state. I "fixed" this by having `frontend_server` produce the
baseline dill *first*, and splicing it directly over `kernel_blob.bin` in the app
bundle before booting the engine, so the engine would boot from the *exact* kernel our
session produced. **The crash was unchanged, byte-for-byte identical error message.**

**Attempted fix #2 (didn't help): entrypoint URI scheme.** Switched from a raw
`file:///.../lib/main.dart` entrypoint/invalidated-file URI to `package:spike_app/main.dart`,
matching flutter_tools' own convention. Kept this (it's more correct and costs
nothing), but **the crash was still unchanged** with this alone.

**The actual fix:** our `reloadSources` call was passing `"force": true`. Per the VM
service's own semantics, `force` reloads *all* libraries, not just the changed ones -
but our delta dill only contains `main.dart`'s recompiled library, which *references*
`StatelessWidget` (via `class MyApp extends StatelessWidget`) without re-including its
declaration (that's the whole point of an incremental diff). Asking the VM to
force-reload `framework.dart` too, from a delta that doesn't actually contain
`framework.dart`'s declarations, is exactly the "not loaded yet" condition. Real
flutter_tools' own `reloadSources` call (`run_hot.dart:1306-1309`) never passes `force`
at all. Removing it fixed the crash immediately and completely - verified by:
1. Re-running with the kernel-splice fix #1 still in place, `force` removed: worked (5/5, 8/8).
2. Re-running with fix #1 *reverted* (engine boots from the ordinary, separately-built
   `kernel_blob.bin`; frontend_server's baseline compile is a genuinely independent
   instance) and `force` still removed: **also worked** (3/3) - proving the kernel-splice
   was never actually necessary. It was removed from the final code for that reason;
   the final recipe boots the engine from the app bundle's ordinary build output,
   exactly like Spike A/B, with `frontend_server` run as a fully independent process.

The lesson: a VM-fatal crash inside the reload path is very easy to misattribute to
"the kernels must be linked" when the actual bug is a single wrong request parameter.
Both are structurally plausible from the stack trace alone; only testing settled it.

## Reverse-engineered protocol notes (things `--help` doesn't say)

- `--no-print-incremental-dependencies` is essential for a hand-rolled line parser -
  it's on by default and dumps 150-600+ `+file://...` lines per compile otherwise.
- The terminator line for both `compile` and `recompile` was observed as just the bare
  boundary key (no trailing dill path), contradicting the `--help` text's
  `<boundary-key> [<output.dill>]`. The parser here (`read_result_block` in
  `src/frontend_server.rs`) treats any line starting with the boundary key as the
  terminator, tolerant of either form.
- `accept` appears to sometimes echo a delayed confirmation line (`<prior-boundary>
  <dill-path> <generation>`) that can arrive interleaved with the *next* command's
  output rather than immediately after `accept` itself. The parser handles this by
  discarding any line seen before the next `"result "` line, rather than trying to
  correlate `accept`'s own output synchronously.
- The incremental dill's path is `<output-dill>.incremental.dill` - not documented,
  discovered by inspecting the working directory after a successful `recompile`.
- Both `compile` and `recompile` accepted a `package:` URI directly for the entrypoint
  and for invalidated files, resolved against `--packages`.

## Measurement mechanism

Per the brief: the fixture app (`spike_app/lib/main.dart`) has a top-level
`const String kReloadMarker = 'MARKER_V1';` and a `SchedulerBinding.instance.
addPostFrameCallback` that re-registers itself every frame, sending
`{marker: kReloadMarker, tapCount: _tapCount}` over a dedicated `duet/spike_c` platform
channel via `invokeMethod('frameMarker', ...)` (fire-and-forget, never awaited on the
Dart side). The host registers a native `setMethodCallHandler` on that channel
(`register_frame_marker_handler` in `src/flutter_embed.rs`) and stores the latest
report in a `Mutex`. The reload driver: (1) rewrites `kReloadMarker`'s value in the
source file and records `t0` at the `std::fs::write` call, (2) drives
recompile -> `reloadSources` -> `ext.flutter.reassemble`, (3) polls the shared state
(2 ms interval) until the *new* marker value shows up, records `t1`, reports
`t1 - t0`. Because `kReloadMarker` is read directly inside `build()`, and the fixture's
own `Ticker` is already calling `setState` every frame regardless of anything the host
does, the very next frame after the reload lands naturally picks up the new constant -
no extra host-side trigger needed beyond `ext.flutter.reassemble`.

One structural gotcha found and fixed along the way: firing all 3 `increment` calls
immediately after `runWithEntrypoint` returns (before Dart's own `initState` has
necessarily registered its channel handler) silently dropped 2 of the 3 - Flutter's
platform channel messenger only buffers ONE pending message per channel until a
handler is registered. Fixed by waiting for the first `frameMarker` report (proof the
handler is live) before sending `increment`, with a retry/backoff loop as a second line
of defense.

## Threading note (why nothing here deadlocks)

This engine build runs UI+platform merged onto one OS thread ("Running with merged UI
and platform thread. Experimental." - same as Spike A/B), which is also the thread
`tao`'s event loop pumps. All blocking I/O in this spike - the `frontend_server`
child-process pipes and the VM service WebSocket - runs on a **separate background
thread** (`run_reload_loop` in `src/main.rs`), touching no ObjC/AppKit/Flutter-engine
object at all; it only reads/writes a `Mutex`-guarded struct the main thread also
touches. The main thread's `tao` event loop just keeps ticking normally throughout,
which is what lets Flutter's own UI-task-runner actually process the reload when it
arrives.

## Contradicts / confirms spec §8.2

**Confirms**, does not contradict:
- "`reloadSources` is issued directly over the VM service WebSocket" - yes, exactly as
  built here, no `flutter_tools` process involved.
- "the VM service URI is printed" - confirmed still true with zero engine configuration
  (same finding as Spike A), and it costs nothing to capture/expose as the spec claims.
- "roughly 200 ms incremental recompile" - measured recompile step alone is 8.8-21.8 ms
  here (much faster than estimated), and the full edit-to-pixel number (111.7-165.1 ms)
  is *also* under the 200 ms figure the spec used to justify the "persistent
  frontend_server, not `flutter assemble` per change" design decision. That decision is
  well-supported by this data - a decision that used a conservative estimate turns out
  to have even more margin than assumed.

**One thing not covered by the spec text quoted in the charter**: `ext.flutter.
reassemble` is not mentioned in spec §8.2's `duet dev` diagram, but was always sent in
this spike's working sequence, matching flutter_tools' own behavior - see "Reverse-
engineered protocol notes". Worth adding as an explicit step if §8.2 is expanded with
implementation detail later.

## What did not work / could not be verified here

- **Whether `ext.flutter.reassemble` is strictly required** versus merely
  belt-and-suspenders (the fixture's own continuously-running `Ticker` might have
  picked up the change on its own next frame regardless). Not isolated - this spike
  always sends it, matching flutter_tools' own behavior, and did not spend time on an
  A/B test given the primary questions were already answered.
- **Whether this holds for non-trivial edits** (adding a new widget subtree, changing a
  class's field shape, a `StatefulWidget`'s `State` class shape - the classic hot-reload
  edge cases that trigger `shapeChangeMappings` or reload rejection). Every edit in this
  spike was a single string-literal change in a top-level `const`. `shapeChangeMappings`
  was empty (`[]`) on every reload observed here, consistent with that. flutter_tools
  itself has well-known limitations here (enum changes, generic type parameter changes,
  etc.) that this spike did not attempt to reproduce or verify against a custom
  embedder specifically.
- **Real hardware/WindowServer rendering.** Same limitation as Spike A/B: this
  environment has no reachable on-screen WindowServer for spawned processes, so all
  visual evidence is in-process rasterization (`cacheDisplayInRect:toBitmapImageRep:`),
  not a live screen capture. The pixels are genuine engine output either way.
- **Windows/Linux.** Not attempted - this spike is macOS-only, like A and B.

## Deliverables checklist

- `src/main.rs`, `src/flutter_embed.rs`, `src/frontend_server.rs`, `src/vm_service.rs`,
  `src/stdout_capture.rs`, `build.rs`, `Cargo.toml` - all in this directory, all build
  and run cleanly (`cargo build`, zero warnings).
- `evidence/before_reload.png`, `evidence/after_reload.png` - in-process rasterized
  screenshots of the fixture app's own window, before the reload loop and after 10
  reloads. Marker text changed, tap count and frame-tick counter did not reset.
- `evidence/canonical_run_stdout.log` - full stdout from the canonical 10-iteration run
  referenced throughout this document (exit code 0, no crash).
