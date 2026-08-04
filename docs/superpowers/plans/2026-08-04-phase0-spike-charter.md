# Phase 0 — Spike Charter

> **Not a TDD plan.** Spikes answer yes/no questions with throwaway code. All code
> produced here is deleted or quarantined under `spikes/`; none of it ships.

**Goal:** Answer three questions that the rest of Duet assumes are "yes", cheaply enough
that a "no" kills or reshapes the project in week one rather than month three.

**Rule:** Each spike is timeboxed. When the box expires, write the finding and stop —
including "inconclusive". A spike that overruns has already told you the answer is "harder
than assumed", which is itself a finding.

**Environment note:** Host is macOS 26.5.2 on arm64. Flutter is on the **master** channel
(3.47.0-0.3.pre, Dart 3.13.0). Only `darwin-x64` engine artifacts are cached and
`FlutterMacOS.framework` is absent. Run this before Spike A or B or they will fail for
unrelated reasons:

```bash
flutter precache --macos && find "$(dirname "$(dirname "$(which flutter)")")/bin/cache/artifacts/engine" -name "FlutterMacOS.framework" -maxdepth 3
```

Expected: at least one path printed. If none, stop and resolve artifact acquisition first.

---

## Spike A — Engine-first embedding and view parenting

**Timebox:** 3 days.

**Question:** Can a Rust process create a Flutter engine *without* a view, then create,
attach, detach, and destroy views against that engine independently — on all three
platforms?

This is the load-bearing assumption behind `FlutterHost` in spec §6.1. Every embedder has a
"view controller creates the engine for you" convenience path; Duet needs the explicit
engine-first path, because attach/detach independence *is* the teardown feature.

**Exit criteria — all must hold:**

1. A Rust binary boots a Flutter engine with no view attached and the process stays alive.
2. A view is created, parented into a `tao` window, and renders a Flutter widget.
3. The view is detached and destroyed while the **engine keeps running**.
4. A second view is created against that same engine and renders.
5. The engine is shut down cleanly with no crash on process exit.

**Record for each platform:** exact engine-first entry point names and signatures. Spec §6.1
flags these as unverified; this spike replaces the guess with fact and unblocks the Phase 3
and Phase 5 plans.

**Order:** macOS → Linux → Windows. Linux second, deliberately: it is the worst case for
Spike B and you want that signal early.

**A "no" means:** `FlutterSurface` cannot attach/detach views against a persistent engine,
so teardown must destroy the whole engine every time. Recoverable — spec §6.4's
"N engines" fallback becomes the only strategy — but it raises resume cost and must be
measured before promising anything about resume latency.

---

## Spike B — Run loop coexistence

**Timebox:** 2 days.

**Question:** Can `tao`'s event loop, the Flutter platform thread, and a `wry` webview
coexist on one OS main thread without deadlock, dropped input, or starvation?

Spec §6.2 asserts all three require the main thread and that `tao`'s `EventLoopProxy` can be
the single cross-platform marshalling mechanism. Both halves need proof.

**Exit criteria:**

1. A `tao` window with a Flutter view and a separate `tao` window with a `wry` webview run
   simultaneously, both interactive.
2. Keyboard and mouse input reach the correct window; neither starves the other.
3. A message posted from a background thread via `EventLoopProxy` is observed on the main
   thread and can drive both a Flutter platform-channel send and a webview `eval`.
4. Runs for 10 minutes with continuous interaction, no deadlock.
5. **Linux specifically:** Flutter's GTK embedder and WebKitGTK coexist in one GTK main
   loop. This is the least-trodden combination in the whole design.

**A "no" means:** the single-process host model in spec §2 is unsound and the multi-process
supervisor — rejected during design — must be reconsidered. This is the highest-consequence
spike; a failure here invalidates the architecture, not a detail of it.

---

## Spike C — Hot reload in a custom embedder

**Timebox:** 4 days. **Highest risk in the project.**

**Question:** Can a Rust-hosted Flutter engine in JIT mode be hot-reloaded via the Dart VM
service, driven by a persistent `frontend_server` we manage ourselves?

Flutter's tooling assumes it owns the runner. Spec §8.2 makes hot reload parity a v1
requirement, so this is a requirement, not a nice-to-have.

`frontend_server_aot.dart.snapshot` is confirmed present at
`<flutter>/bin/cache/dart-sdk/bin/snapshots/frontend_server_aot.dart.snapshot`.

**Exit criteria:**

1. `flutter assemble` produces a debug (JIT) `kernel_blob.bin` plus `flutter_assets` that a
   Rust-hosted engine loads and renders.
2. The host prints a Dart VM service URI, and `flutter attach` connects to it.
3. A persistent `frontend_server` process accepts an incremental `recompile` request and
   emits an incremental kernel diff.
4. A `reloadSources` RPC over the VM service WebSocket applies that diff, and the change is
   visible on screen **without losing Dart heap state**.
5. Measured incremental edit→pixel latency is **under 500 ms**.

Criterion 5 is the point of the spike. A reload that works but takes 3 seconds is a rebuild
wearing a costume, and would not satisfy the parity requirement that motivated this scope.

**A "no", or latency above ~1 s, means:** reopen the dev-loop decision. The fallback —
hot restart reusing the teardown/re-attach machinery, with shared state surviving — was
identified during design as nearly free. Losing hot reload is a real adoption cost but not
a structural one.

---

## Output

One file, `docs/superpowers/spikes/2026-08-04-phase0-findings.md`, containing per spike:

- Verdict: **yes** / **no** / **inconclusive (timebox expired)**
- For Spike A: the confirmed engine-first API signatures, per platform
- For Spike C: measured edit→pixel latency
- Anything discovered that contradicts the spec, with the section number

**Gate:** Phase 3 and Phase 5 plans are not written until Spike A and B report. Phase 4 is
not written until Spike C reports. Phase 1 does not depend on any of them and starts
immediately in parallel — `duet-core` is platform-free precisely so this parallelism exists.
