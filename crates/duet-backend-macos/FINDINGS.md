# Phase 2b-3 — `duet-backend-macos`: findings from running it for real

> **UPDATE (branch `fix/flutter-lifecycle-on-detach`): F1's hang is fixed.**
> `FlutterEngine::detach`/`FlutterEngine::attach` now send real
> `AppLifecycleState` transitions on `flutter/lifecycle` around the
> view-detach/attach points. The run-loop starvation is gone, the `Could not
> create the embedder backing store` spam is gone (exactly one line per run,
> not thousands), and `examples/lifecycle.rs` now completes in ~1.7 s instead
> of hanging forever — the engine genuinely reaches `Teardown`/`Report` for
> the first time. See **F5** below for the full re-measurement, the
> before/after comparison, and one genuine remaining problem: the example's
> own `assert!` still fails, for a reason unrelated to F1 (a baseline-sample
> quirk in `examples/lifecycle.rs`, not a reclaim-mechanism problem — F5
> explains and quantifies it). The rest of this document (through F4) is left
> exactly as originally written, as the historical record of the bug this
> fixes.

**Overall verdict (as originally written, before the fix above): the reclaim
mechanism itself works — Spike A already measured that — but this phase's own
real run could not reach it.** `examples/lifecycle.rs`
drives one real Flutter surface through `Start` and `Suspend` over the real
`duet-host`/`duet-supervisor`/`duet-runtime` orchestration, and hangs
indefinitely at the point `duet-supervisor`'s own design requires: the run-loop
turn between `Suspend` (view detached, engine alive) and `Teardown` (engine
destroyed). **Never verify a pass you did not observe** — this document does not
claim the 80 MB floor was cleared, because it was never measured end to end here.

| # | Claim | Verdict | Evidence |
|---|---|---|---|
| 1 | Engine boots, view attaches and renders | **yes** | `Readiness::Ready` returned; PNG below shows real Flutter content |
| 2 | `ProxySink` delivers a real write onto the UI thread | **yes** | `[lifecycle] ProxySink delivered 1 notification(s)`, every run |
| 3 | `Suspend` detaches the view, engine stays alive | **yes** | RSS barely moves across detach (221,472 -> 221,904 kB) |
| 4 | `Teardown` destroys the engine and reclaims >=80 MB | **cannot verify here** | the run never reaches `Teardown` — see F1 |
| 5 | Real keyboard/mouse input | **cannot verify here** | no WindowServer on this machine (Phase 0 finding, still holds) |
| 6 | `Could not create the embedder backing store` appears | **yes, unboundedly** | 162,686 occurrences in 90 s in one run — contradicts Spike A's "harmless" read, see F1 |

Environment: macOS 26.5.2 (25F84), arm64. `tao` 0.36.0, `objc2` 0.6.4
(`catch-all`), `wry` 0.56.0. Flutter fixture: `spikes/spike_app`
(`App.framework` `CFBundleShortVersionString` 1.0), the same one Spikes A, B
and C used.

---

## F1 — The single most important finding: `Suspend` triggers a runaway, apparently unbounded retry storm that starves the host's own run loop

This is not a bug in `examples/lifecycle.rs`'s driver logic, and not something a
different scheduling choice inside this crate can route around — see the four
independent reproductions and the root-cause analysis below.

### What was observed

Running `cargo run -p duet-backend-macos --example lifecycle` exactly as Task 4
specifies (`Policy::OnLastWindowClosed { grace_ms: 500 }`, the real
`spike_app` fixture) reliably reaches four real, correct stages and then stops
making progress permanently:

```
[t+  0.03s rss=   37104kB] process start, no surface registered
[t+  0.28s rss=  159728kB] renderer started, view attached (Readiness::Ready)
[t+  0.83s rss=  221472kB] after rasterizing the attached view
[t+  0.89s rss=  221904kB] view detached, suspending (engine still alive)
```

The very next line in the log — not a later one, the *immediately following*
line — is:

```
[ERROR:flutter/shell/platform/embedder/embedder.cc(1406)] Could not create the embedder backing store.
```

...repeating forever. This crate's own `[lifecycle]` progress prints (which log
every `Host::tick` call and its returned actions) stop dead at that point: no
`AwaitTeardown` tick, no report, no exit. Confirmed across **four independent
runs**:

| Run | Real time held open | Outcome | CPU | RSS at kill | Error lines |
|---|---|---|---|---|---|
| 1 | ~60 s (timeout) | no `Teardown` tick | ~111% | not sampled | ~20,000+ (log truncated) |
| 2 | ~5 min | no `Teardown` tick | 110.9% | 310,288 kB (from 217,280 kB at detach) | 535,973 |
| 3 (accelerated grace, see below) | ~2 min | no `Teardown` tick | 111.2% | not re-sampled | many |
| 4 (final, shipped config) | 90 s (deliberately killed) | no `Teardown` tick | 112.5% | 297,408 kB (from 221,904 kB at detach) | 162,686 |

RSS **grows** during the hang rather than shrinking — the opposite of what a
"suspended, idle" engine should do — which is itself worth flagging separately
from the CPU spin.

### Root cause, as far as it can be established without a debugger attach

`spikes/spike_app/lib/main.dart` runs a `Ticker` (`_ticker = createTicker(...);
_ticker.start();`) that requests a new frame on every vsync, unconditionally,
for the lifetime of the widget — this fixture was built for Spike C's
hot-reload latency measurement, which needed continuous frame production, not
as a static counter app. The Ticker does not know or care whether a view is
currently attached to its engine.

The moment `duet_backend_macos::backend::MacBackend::detach_view` removes the
Flutter `NSView` from its superview (the `Suspend` action, which
`Policy::OnLastWindowClosed`'s grace period always performs before `Teardown`
— see `duet-supervisor`'s `decide()`), the engine's next scheduled frame has
nowhere to composite into. It logs `Could not create the embedder backing
store` and, evidently, retries immediately rather than backing off or
yielding — this process never returns to `tao`'s run loop afterward in any of
the four runs, which is what stops our own `ControlFlow::WaitUntil` timer from
ever firing again.

**This machine's lack of a real on-screen WindowServer (established in Phase
0) is very likely a necessary ingredient, not just background context.**
Without a real display, there is no real vsync signal to pace the Ticker's
retries; the failed-frame path appears to have no independent backoff, so
without external pacing it can retry as fast as the CPU allows — consistent
with the observed ~1,500-13,000 error lines/second and 100%+ sustained CPU.

### Why this could not be dodged by waiting less

A hypothesis worth ruling out: maybe the storm only *builds up* over real time,
and ticking `Teardown` sooner would beat it. This was tested directly (run 3
above): the driver was changed so the tick that checks for grace expiry ran on
the very next run-loop turn (tens of milliseconds of real time), passing a
`duet_core::Instant` already past the configured grace deadline — legitimate,
since `duet-supervisor`'s time is entirely caller-supplied and never reads a
real clock (see its module docs). This did **not** avoid the hang: by the time
that very next turn tried to run, the storm was already established and the
turn never happened. The error begins on the line immediately following
detach in every run, with no observed delay — so the window to "beat" it, if
one exists at all, is smaller than one run-loop turn can reliably hit. This
temporary change was reverted; the shipped example uses the real, plan-specified
`grace_ms: 500` with a real wall-clock wait.

### Why this is structural, not just this example's driver being wrong

`duet_supervisor::Supervisor::tick`'s `decide()` produces at most one
`SurfaceAction` per surface per call by construction (see
`duet-supervisor/src/supervisor.rs`), and `MacBackend`'s own docs (`src/backend.rs`)
document that `Suspend` (detach) and the later `Resume`/`Teardown` for the same
surface are therefore guaranteed to land in **separate** `Host::tick()` calls —
this is exactly what Spike A's constraint 5 (detach -> recreate must cross a
run-loop tick) requires to avoid the "engine already has a view controller"
exception. That guarantee is correct and load-bearing for the reasons already
documented in `src/backend.rs`. But it also means there is *always* at least
one full run-loop turn between "view detached, engine alive" and "engine
destroyed" — precisely the window in which F1's storm establishes itself for a
continuously-animating surface. The two requirements are in real tension:
safety around engine reuse requires a turn boundary; this fixture's Ticker
punishes any turn boundary with no attached view.

### Contradicts a Phase 0 finding

Spike A's `FINDINGS.md` (section "Anything else that surprised me", last
bullet) observed the same `Could not create the embedder backing store`
message during its 8-cycle leak check and concluded it "looks like a harmless,
logged-but-recovered internal race" — because a *new* view arrived roughly
every 170 ms in that loop, which (per this phase's findings) is apparently
enough to end the storm before it was ever measured. Once a view stays absent
for any sustained period — which is the entire point of `OnLastWindowClosed`'s
grace period, and of `Suspend` generally — the same message is not harmless at
all: it appears to correlate with, or cause, indefinite CPU/run-loop
starvation. **Spike A's "harmless" characterization should be considered
retracted** for any scenario where a view stays detached longer than roughly
one run-loop turn.

### What this does *not* call into question

Spike A's actual measurement that `shutDownEngine` reclaims memory (223 MB ->
104 MB) is unaffected by this finding — that measurement was taken with the
engine given the chance to actually run `shutDownEngine`, which this phase's
driver could never do. The mechanism this phase set out to prove
(`FlutterEngine::shutdown`, `MacBackend::destroy_renderer`) is untouched code
from Task 2/3, ported faithfully from Spike A, and there is no reason from
this investigation to doubt it *would* reclaim memory if it ever ran. What
could not be established here is whether it reliably gets the chance to run
against an app that keeps animating through its grace period.

---

## F2 — What was verified end to end

### RSS across the four stages actually reached

| Stage | RSS (kB) | Note |
|---|---|---|
| Process start, no surface registered | 37,104 | baseline |
| Renderer started, view attached (`Readiness::Ready`) | 159,728 | engine boot + `attach_view`, one `Host::tick` |
| After rasterizing the attached view | 221,472 | Dart isolate/Skia warm-up continues after attach, matching Spike A's shape |
| View detached, suspending (engine still alive) | 221,904 | essentially unchanged from the previous row — matches Spike A's finding that `detach` alone reclaims nothing |

The **80 MB-floor delta this phase exists to measure (view attached -> after
teardown) could not be computed**, because "after teardown" was never reached.
The `assert!` in `print_report` — which would print PASS/FAIL and enforce the
floor — never runs in this environment; `cargo run -p duet-backend-macos
--example lifecycle` hangs rather than exiting, as documented in the example's
own module docs.

### The rasterized PNG

`cacheDisplayInRect:toBitmapImageRep:` (Spike A's technique, no on-screen
WindowServer needed) was exercised on every run. Results were **not fully
deterministic**: the first run captured before this write-up produced an
all-black 800x600 PNG (10,096 bytes); the final run captured a fully-rendered
frame (39,133 bytes) showing the actual `spike_app` UI — title bar reading
"Duet Spike B - Flutter side", live text "marker: MARKER_V1", "frame ticks: 2",
"pings received: 0", "taps: 0", and the floating action button. Both runs
waited the same nominal ~0.55 s between attach and rasterize; the difference is
most plausibly Dart isolate/JIT warm-up variance in how many frames have
actually painted by that point, not a bug in the rasterization call itself —
when it captures after the first paint, it is unambiguously real content, not
a placeholder or an artifact of the capture method.

**Practical implication for a later phase:** do not rasterize on a fixed short
delay after attach; wait for an actual "first frame" signal, or simpler, just
retry rasterization a few times, rather than assuming ~0.5 s is always enough.

### `ProxySink` delivery

Confirmed on every run: after subscribing the surface's `SubscriberId` to
`zoom` and writing through `StoreHandle::set`, the log shows
`[lifecycle] ProxySink delivered 1 notification(s) onto the UI thread` before
the surface is ever suspended — a real write, from `duet-runtime`'s core
thread, marshaled through `tao`'s `EventLoopProxy` and observed as
`Event::UserEvent(DuetEvent::Notifications(_))` on the UI thread, carrying the
correct path. This corroborates Spike B's 709/709 measurement with a live
`duet-runtime` core thread and a live Flutter engine in the loop, not just a
synthetic proxy round-trip.

### `start_renderer`'s `Readiness`

`MacBackend::start_renderer` (`src/backend.rs`) returns `Readiness::Ready`,
never `Readiness::Pending`, and every run's log confirms `Host::perform_start`
attaches the view synchronously in the same `Host::tick()` call that started
it (`[lifecycle] tick at open produced [Start(SurfaceId(0))]` followed
immediately by a successful `attach_view` — no separate `HostEvent::Ready`
report was ever needed). This matches the plan's own hypothesis: Spike A
established `runWithEntrypoint:` is synchronous (returns only once the isolate
is actually running), so `Ready` is honest for a Flutter renderer specifically.
A `wry` webview surface (not implemented in this crate — see the crate root
docs on scope) would warrant `Pending`, since `load_url` returns before the
page finishes loading; that remains a documented hypothesis, not something
this phase could measure, since no webview surface kind exists here yet.

---

## F3 — What could not be verified here, and why

- **The RSS reclaim floor (80 MB) itself.** See F1. The mechanism Spike A
  measured is untouched, but this phase's own end-to-end run of it through
  `duet-host` orchestration never completed.
- **Real keyboard or mouse input.** Unchanged from Phase 0: this machine has
  no reachable on-screen WindowServer for spawned processes. Spike B found
  synthetic input reaches a Flutter view but not a `WKWebView`; that asymmetry
  was not re-tested here (this crate implements no webview surface yet) and
  remains open.
- **Anything a human would judge by looking at a screen.** Same constraint as
  every phase since Spike A; the rasterized-PNG substitute is the best
  available proof, and F2 documents its own reliability caveat.
- **Whether the F1 hang is specific to this fixture's `Ticker`, to the "merged
  UI and platform thread. Experimental." threading mode logged at every
  engine boot, to the lack of a real vsync source, or to some combination.**
  Distinguishing these would need either a non-animating Flutter fixture
  (out of scope — `spikes/**` is not touched by this phase) or a debugger
  attached to the hung process to see exactly what the embedder's retry path
  is doing between failed attempts; neither was available here.
- **Whether the hang would reproduce on a machine with a real WindowServer**,
  where the Ticker could get genuine vsync pacing instead of (apparently)
  retrying unthrottled. This is the single most valuable follow-up
  measurement for whoever has access to such a machine.

---

## F4 — A pre-existing, unrelated doc-link defect found while reading this crate

`cargo doc -p duet-backend-macos --no-deps` (with `RUSTDOCFLAGS=-D warnings`,
matching CI's own flag) fails: `src/backend.rs` has five public doc comments
linking to `crate::engine::FlutterEngine` and its methods, which are
`pub(crate)`/private and so cannot be linked from public docs. This predates
this phase (introduced in Tasks 2/3, commits `f4c4d9b`/`f9b937b`) and was never
caught, because CI's own `cargo doc` step could not even compile this crate on
`ubuntu-latest` to reach the lint. It does **not** affect CI today, because
Task 5's exclusion (`--exclude duet-backend-macos`, see below) means the
`Docs` step never attempts this crate at all — but it is worth fixing
separately since it means `cargo doc -p duet-backend-macos` is currently
broken for any contributor working on this crate locally.

---

## F5 — The F1 fix, and what re-running `examples/lifecycle.rs` actually shows

### Root cause, confirmed against the Flutter framework source

F1 happens because Dart's scheduler keeps requesting frames after its view is
detached. `spikes/spike_app`'s `Ticker` (`lib/main.dart`) asks for a new frame
on every vsync unconditionally, forever — it has no way to know a view was
just removed. With no view to composite into, the embedder logs `Could not
create the embedder backing store` and retries, apparently without ever
yielding back to `tao`'s run loop.

Reading the Flutter framework source (`/Users/kishan/dev/rkishan516/flutterDC`)
confirms the fix: `flutter/lifecycle` is a `BasicMessageChannel<String?>`
using `StringCodec` (`packages/flutter/lib/src/services/system_channels.dart:386`),
so the wire payload is just the raw UTF-8 bytes of a string like
`"AppLifecycleState.hidden"` — no method-call envelope, no codec object
needed on the Rust side.
`SchedulerBinding.handleAppLifecycleStateChanged`
(`packages/flutter/lib/src/scheduler/binding.dart`) is what actually disables
frame scheduling, and only on `AppLifecycleState.hidden`
(`_setFramesEnabledState(false)`) — `resumed` and `inactive` leave frames
enabled. Critically, the transition table in
`packages/flutter/lib/src/services/binding.dart` does **not** allow jumping
straight from `resumed` to `hidden` (only `resumed -> inactive`,
`inactive -> resumed | hidden`, `hidden -> paused | inactive`,
`paused -> hidden | detached`) — skipping the intermediate `inactive` step
trips a framework assertion instead of silently working.

### The fix

`FlutterEngine::set_lifecycle_state` (`src/engine.rs`) sends a raw UTF-8
string on `flutter/lifecycle` through the engine's `binaryMessenger`, reusing
the same `sendOnChannel:message:` primitive Spike B's method channel already
proved works (`spikes/spike-b-macos/src/main.rs`).

- **`FlutterEngine::detach`** now sends `AppLifecycleState.inactive` then
  `AppLifecycleState.hidden` *before* removing the view and dropping the
  controller — stopping frame scheduling before the view disappears, rather
  than after.
- **`FlutterEngine::attach`** now sends `AppLifecycleState.inactive` then
  `AppLifecycleState.resumed` *after* the view is parented back in —
  restarting frame scheduling.
- Both call sites go through `inactive` as the mandatory intermediate step in
  both directions (`resumed -> inactive -> hidden` and
  `hidden -> inactive -> resumed`), per the transition table above.

**`detach`'s signature changed from infallible to `Result<(), BackendError>`.**
Sending a lifecycle message is a real Objective-C call that can throw, unlike
the old body (`removeFromSuperview` only). The chosen approach: `detach`
*always* performs the structural detach (`removeFromSuperview` + dropping the
controller run unconditionally, and that failure path stays absorbed exactly
as before, since dropping the controller happens regardless of it), but now
returns `Err` if either lifecycle send failed, because — unlike a failed
`removeFromSuperview` — a failed lifecycle send has a real consequence: F1's
retry storm may not actually be prevented that time. `MacBackend::detach_view`
(`src/backend.rs`), which already returned `Result`, now just propagates it.
`FlutterEngine::shutdown` (still infallible, for the same "no distinct
recovery path, engine is dropped by the caller right after regardless" reason
as its `shutDownEngine` call) absorbs `detach`'s `Result` with `let _ =
self.detach();` rather than letting it change `shutdown`'s own signature.

### Independent confirmation: the Dart side actually observed the transition

`spikes/spike_app/lib/main.dart` was extended with a `WidgetsBindingObserver`
that prints `didChangeAppLifecycleState`. A real run shows, in order:

```
flutter: [spike_app] didChangeAppLifecycleState: AppLifecycleState.inactive
flutter: [spike_app] didChangeAppLifecycleState: AppLifecycleState.hidden
[lifecycle] tick at close produced [Suspend(SurfaceId(0))]
```

Both `println!`s from the Dart side land *before* the Rust driver's own
`tick at close produced` line prints — i.e., synchronously, within the same
`Host::tick()` call that executes `Suspend`, consistent with this process
running Flutter's "merged UI and platform thread" mode. This is a stronger
proof than "the storm didn't happen": the framework demonstrably received and
processed both transitions in the correct order.

### Before / after: the spin itself

| | Before (original F1 finding) | After (this fix, 4 runs) |
|---|---|---|
| `Could not create the embedder backing store` count | 162,686 in 90 s (one run); up to ~13,000/s | **exactly 1**, every run |
| CPU | ~111%, sustained | transient (engine boot/render work only), decays to idle; no sustained pinning |
| RSS during "suspended" | **grows**: 221,904 -> 297,408 kB over 90 s | stable/shrinks: proceeds straight to teardown |
| Reaches `Teardown`/`Report`? | **never** (killed after up to ~5 min) | **yes**, every run |
| Wall time to complete | never (hang) | ~1.6-1.75 s |

CPU was independently sampled every 0.1 s across one full run (`ps -o
%cpu,rss -p <pid>`): it ramps to ~85-96% for roughly half a second during
normal engine boot/Skia warm-up (expected — this is the same cost every
engine boot pays, not a storm), then decays through 70% -> 48% -> 30% -> 19%
-> 12% as the process winds down toward `Teardown`/exit, rather than staying
pinned near 100%+ indefinitely as F1 originally measured.

### RSS table (`examples/lifecycle.rs`, 4 runs, this fix applied)

All figures in kB, via `ps -o rss=`:

| Stage | Run 1 | Run 2 | Run 3 | Run 5 (with observer) |
|---|---:|---:|---:|---:|
| process start | 37,312 | 37,376 | 37,232 | 36,992 |
| renderer started, view attached (`Readiness::Ready`) | 159,776 | 160,192 | 160,000 | 159,936 |
| after rasterizing the attached view | 222,064 | 229,328 | 222,448 | 220,816 |
| view detached, suspending (engine alive) | 222,432 | 227,456 | 220,208 | 218,512 |
| torn down (engine shut down) | 96,208 | 102,992 | 96,576 | 95,664 |

### The measured delta, and the assertion's actual result — reported honestly

`print_report` (`examples/lifecycle.rs`) computes its delta as **"renderer
started, view attached"** (the *first* post-attach sample, taken before the
Dart isolate/Skia warm-up that F2 already documented continues after attach)
minus **"torn down"**:

| Run | attached (early) kB | torn down kB | delta | floor | Result |
|---|---:|---:|---:|---:|---|
| 1 | 159,776 | 96,208 | 63,568 | 81,920 | **FAIL** |
| 2 | 160,192 | 102,992 | 57,200 | 81,920 | **FAIL** |
| 3 | 160,000 | 96,576 | 63,424 | 81,920 | **FAIL** |
| 5 | 159,936 | 95,664 | 64,272 | 81,920 | **FAIL** |

**The example's own `assert!` fails, every run, and the process exits via
panic (exit code 101) rather than a clean `PASS`.** This is reported exactly
as measured — the assertion did not pass, and this document is not going to
call that anything other than what it is.

That said, this is **not** evidence that `shutDownEngine` fails to reclaim
memory to the degree Spike A measured. The reason is the baseline `print_report`
diffs against: "view attached (`Readiness::Ready`)" is sampled immediately
after `attach_view` returns, *before* the Dart isolate/Skia warm-up that grows
RSS by another ~60 MB during the subsequent `Rasterize` step (F2, unchanged by
this fix) — a jump that already happens in this codebase, both before and
after this fix, and is not part of what teardown is being asked to undo.
Spike A's own 234 MB -> 108 MB (~126 MB drop) measurement
(`spikes/spike-a-macos/FINDINGS.md`) was taken from a settled, warmed-up RSS
right before `shutDownEngine`, not from immediately after the first attach —
so the apples-to-apples comparison is "view detached, suspending" (the last
sample taken before `Teardown` actually runs) minus "torn down":

| Run | detached (pre-teardown) kB | torn down kB | delta |
|---|---:|---:|---:|
| 1 | 222,432 | 96,208 | 126,224 |
| 2 | 227,456 | 102,992 | 124,464 |
| 3 | 220,208 | 96,576 | 123,632 |
| 5 | 218,512 | 95,664 | 122,848 |

Measured this way, every run clears the 80 MB (81,920 kB) floor comfortably —
122.8-126.2 MB reclaimed, consistent with (and slightly exceeding) Spike A's
~126 MB. **Both facts are real and are being reported separately, as the task
brief for this fix required:** F1's spin is fixed and frame scheduling
genuinely stops (confirmed three independent ways — the storm is gone, CPU is
not pinned, and the Dart side's own `didChangeAppLifecycleState` print
confirms `hidden` was received) — but the specific `assert!` this example
ships with fails on every run, because it diffs against a pre-warm-up
baseline sample rather than the last sample before teardown. This is a
pre-existing measurement-baseline choice in `examples/lifecycle.rs` (shipped
in the commit that added Task 4, before this fix), not something this fix
introduced or attempted to paper over by changing the assertion to make it
pass.

### The unrelated `MissingPluginException` noise

Every run also logs several `MissingPluginException(No implementation found
for method frameMarker on channel duet/spike_c)` errors. This is `spike_app`'s
Spike-C hot-reload probe (`_reportFrame` in `lib/main.dart`) calling a channel
this Rust harness never registers a handler for. It is unrelated to F1 (it
was already firing before this fix, just drowned out by the backing-store
storm) and does not affect the run's completion — Dart's
`MethodChannel.invokeMethod` treats a missing handler as a normal (if noisy)
error, not a fatal one. Not touched by this fix; noted here for completeness
since it is visible in every log excerpt above.

### Commands run

```
cargo build -p duet-backend-macos --example lifecycle
cd spikes/spike_app && flutter build macos --debug   # after editing lib/main.dart
cargo run -p duet-backend-macos --example lifecycle   # x4, plus one instrumented CPU/RSS sample run
cargo test -p duet-backend-macos                       # 2 passed, 1 ignored (needs main thread)
cargo clippy -p duet-backend-macos --all-targets -- -D warnings   # clean
cargo fmt --all --check                                            # clean
```

---

## CI: crate excluded, not gated down

`ubuntu-latest` (this workspace's only CI runner) has neither a window server
nor `FlutterMacOS.framework`, so `duet-backend-macos` cannot *compile* there,
let alone run or reach the 90% line-coverage gate every other crate clears
(96.07% across the other five, unchanged by this phase — see below). Per the
plan, one of two approaches had to be picked and applied consistently rather
than silently letting CI stop covering the crate while looking green:

**Chosen: `--exclude duet-backend-macos` on every workspace-wide `cargo`
invocation in `.github/workflows/duet.yml` that would try to build it** —
`clippy`, `doc`, `llvm-cov`, and `test`. `cargo fmt --all -- --check` is left
workspace-wide (formatting is pure syntax, needs no compilation, and runs
fine on any platform). The alternative — gating the crate behind
`[target.'cfg(target_os = "macos")'.dependencies]` — was not used, because it
would leave `--workspace` invocations silently skipping the crate on Linux
without a visible, auditable reason at the CI-config layer; the explicit
`--exclude` plus the comment above it in `duet.yml` is the more honest of the
two, and is what the plan's own example diff already showed for the coverage
step specifically.

The 90% coverage threshold was **not** lowered; it still applies to the other
five crates.

## Workspace verification (the other five crates, unaffected)

```
cargo test --workspace --exclude duet-backend-macos --locked         # 100+ tests, all pass
cargo llvm-cov --workspace --exclude duet-backend-macos --locked --fail-under-lines 90
  # TOTAL: 96.07% lines (8266 regions / 330 missed), well above the 90% gate
cargo fmt --all -- --check                                            # clean
git diff --stat main -- crates/duet-core crates/duet-runtime crates/duet-codec crates/duet-supervisor crates/duet-host
  # (no output — these five crates are byte-for-byte unchanged by this phase)
```

---

## Phase 2b-5 — the webview guest speaks `duet-protocol` over real `wry` IPC

This phase wired a `wry` `WebView` into the same `duet-protocol` conversation
`duet-backend-macos`'s Flutter surface already speaks, and proved a value
written by JavaScript is readable from Rust, and a value written by Rust
reaches JavaScript as a push — both over the real transport, not a mock of
it. It also extracted the transport-agnostic half of that work into a new
`duet-webview` crate mid-phase, for a reason explained in F12.

**Where this section lives, and why.** The plan allowed a webview-specific
findings file if that turned out to be the better home for the platform-free
parts. This document keeps everything in one place instead: the routing
logic in `duet-webview` and the `wry`-specific glue in
`duet-backend-macos::webview` were proven together, in one example, and
splitting the writeup across two files would force a reader to hold both
open to see the whole claim (guest text in, JavaScript out) verified end to
end. F9–F11 below cover the platform-free half; F7, F8 and F9's second half
cover what is specific to `wry`/macOS.

Environment: macOS 26.5.2 (25F84), arm64. `tao` 0.36.0, `wry` 0.56.0, `objc2`
0.6.4 (`catch-all`), `serde_json` 1.0.151 — same toolchain F1–F5 used, `wry`
newly exercised via `with_ipc_handler` for the first time.

| # | Claim | Verdict | Evidence |
|---|---|---|---|
| 1 | A JS guest's write is readable from Rust | **yes** | `Rust read counter = Int(42), written by JavaScript over wry IPC`, every run |
| 2 | A Rust write reaches the guest as exactly one push | **yes** | `window.__duet.pushes.length == 1`, path `counter`, value `{t:"i", v:"99"}`, every run |
| 3 | `with_ipc_handler` actually delivers guest IPC to Rust | **yes, first try** | never spike-proven before this phase — see F7 |
| 4 | Dropping the reply-evaluation arm is silently catastrophic | **yes, reproduced independently** | see F8: store write lands, guest hangs forever, only a deadline makes it visible |
| 5 | Hostile guest text cannot panic, hang, or get echoed back unbounded | **yes** | see F11: exact byte counts reproduced |
| 6 | `duet_codec`'s recursive decoder is reachable from guest text | **no** | see F10: `serde_json`'s own 128-level recursion guard rejects it first |
| 7 | Real mouse/keyboard input reaches the webview | **cannot verify here** | no on-screen WindowServer; unchanged from every prior phase |
| 8 | Two live guests' subscriptions are isolated | **only structurally proven** | see F13; no run has had a webview and a Flutter engine live against one store simultaneously |
| 9 | `duet-webview` clears the workspace's 90% line-coverage gate on its own | **no — 89.78%, a real gap** | see F13; the gate still passes because it is workspace-wide |

---

### F6 — The proof, measured

`cargo run -p duet-backend-macos --example webview_state` (`examples/webview_state.rs`)
drives a real `tao` window, a real `wry` `WebView`, and a real
`duet-runtime` core thread through four stages: guest boots, guest writes
`counter = 42` over IPC, Rust reads it back, then Rust writes `counter = 99`
and the guest observes exactly one push carrying it. Reproduced 5 times in a
row after this document's other findings were verified (see F8's sabotage
test and F11's byte-count checks, both of which required rebuilding the same
binary); every run printed the same five `PASS` lines:

```
PASS: guest bootstrap ran - window.__duet exists after 0.20-0.40s
PASS: wry IPC round trip (guest -> host -> guest) - the guest received 1 response(s) over wry IPC
PASS: a JS-written value is readable from Rust - Rust read counter = Int(42), written by JavaScript over wry IPC
PASS: a Rust write reached the guest exactly once - window.__duet.pushes.length == 1 (want exactly 1)
PASS: that push carried the value Rust wrote - the push carried path="counter" value={t:"i", v:"99"} (want counter / i / 99)

ALL PASS: a JavaScript guest and Rust share one store over real wry IPC
```

Per-run timings (`ps`/`time`-independent, taken from the example's own
`Instant`-based prints and from `/usr/bin/time -p` around the whole process):

| Run | boot | 1st reply | 2nd reply | 1st push | total wall time (`time -p`) |
|---|---:|---:|---:|---:|---:|
| 1 | 0.24s | 0.39s | 0.59s | 0.74s | 1.46s |
| 2 | 0.28s | 0.44s | 0.64s | 0.79s | 1.61s |
| 3 | — | — | — | — | 1.50s |
| 4 | — | — | — | — | 1.58s |
| 5 (with `[measure]` instrumentation, see F11) | 0.20-0.40s | — | — | — | — |

**Correcting a number from the task brief:** the brief characterized the
whole run as "~0.85s". That does not hold under measurement here — total
wall time across five runs of the compiled binary was consistently
1.46-1.61s, not ~0.85s. The ~0.7-0.9s figures *are* real, but they mark when
the first push arrives, not when the process exits: `SETTLE_TURNS = 6` holds
the run open for 6 more 50ms turns after that (300ms) before asserting the
count, plus `Runtime::shutdown()` and process exit overhead account for the
rest. Reported here as measured, not as originally claimed — the discrepancy
is in what point of the run the number describes, not a sign anything is
actually slower than expected.

### F7 — `with_ipc_handler` was not spike-proven, and now is

Spike B (`spikes/spike-b-macos/src/main.rs`) exercised `with_html` and
`evaluate_script_with_callback` — JavaScript running, and its return value
coming back to Rust — but never `with_ipc_handler`: nothing in that spike
sent a message *from* the guest unprompted. This phase's `WebviewSurface::new`
(`src/webview.rs:46-63`) is therefore the first thing in this workspace to
register a `wry` IPC handler and observe it actually fire. It worked on the
first real run with no workaround needed — worth recording precisely because
Spike B *did* need a workaround for the adjacent double-encoding problem (see
F9), so "first try, no surprises" for the IPC handler specifically is a real
data point, not a foregone conclusion.

### F8 — A load-bearing arm whose absence is silent, reproduced by sabotage

`WebviewSurface::new`'s IPC handler cannot hold the webview it answers
through (see F9's second half for why), so it posts a
`DuetEvent::WebviewScript(String)` through an `EventLoopProxy` and relies on
the event loop's own `Event::UserEvent(DuetEvent::WebviewScript(js)) =>
app.surface.eval(&js)` arm (`examples/webview_state.rs:292-296`) to actually
run it. This was re-verified here by deliberately deleting that arm's body
(replacing the `eval` call with a comment) and lowering the example's
`DEADLINE` from 30s to 5s so the failure would surface quickly, then
rebuilding and running:

```
[webview_state] window and wry webview created; subscriber=SubscriberId(0)
[webview_state] guest booted; probe readback = {...,"ready":true,...}
[webview_state] evaluated the guest's set call
[sabotage-check] counter in store = Ok(Some(Int(42)))

=== Duet webview shared-state report ===
TIMED OUT after 5s while at stage AwaitSetReply; last readback = {...,"pushLen":0,"logLen":0,...}
notifications received for this subscriber: 0, evaluated into the guest: 0

FAIL: wry IPC round trip (guest -> host -> guest) - never reached; the sequence stopped before this stage
FAIL: a JS-written value is readable from Rust - never reached; the sequence stopped before this stage
4 check(s) FAILED
```

A temporary `[sabotage-check]` print (added only for this verification, then
removed) confirms the mechanism precisely: the store write lands —
`counter` is `Int(42)` in Rust — *before* the sabotaged arm would have run,
because `handle_ipc_text` performs the dispatch synchronously inside the IPC
handler itself, and only the reply script's delivery is what got dropped.
From the guest's point of view there is no error, no rejected promise,
nothing in the console — its `set(...)` call simply never resolves. The
example's own `AwaitSetReply` poll loop has no way to distinguish "still in
flight" from "will never arrive," so only the whole-run `DEADLINE` turns this
into a visible failure at all; a guest with no such deadline (a real webview
UI) would wait forever with no diagnostic. Both edits (the deadline, the
sabotaged arm) were reverted after this measurement; `git diff` against
`examples/webview_state.rs` is empty again (verified below, F-workspace
section).

### F9 — Two design decisions the code carries a comment for, verified against `wry`'s own source

**Replies are pushed into the guest, never returned from an evaluated
script.** `handle_ipc_text`'s doc comment and `WebviewSurface::new`
(`src/webview.rs:52-60`) both explain why: `wry`'s
`evaluate_script_with_callback` on macOS runs the script's return value
through `NSJSONSerialization` before handing it back
(`wry-0.56.0/src/wkwebview/mod.rs:741`, `:759`,
`evaluateJavaScript_completionHandler`) — so a script that returns an
already-stringified JSON string would come back double-encoded. Spike B hit
exactly this. Confirmed directly in the vendored `wry` source at the paths
above; `response_script`/`push_script` sidestep it entirely by embedding the
JSON in the *script text* itself (`window.__duet.onResponse({reply_json});`)
rather than returning it.

**The IPC handler cannot hold the webview.** Read from `wry` 0.56.0's own
signature (`wry-0.56.0/src/lib.rs:1175-1178`):

```rust
pub fn with_ipc_handler<F>(mut self, handler: F) -> Self
where
    F: Fn(Request<String>) + 'static,
```

`Fn`, not `FnMut` — so the handler can only capture things it accesses by
shared reference — and no `Send` bound, just `'static`. `WebviewBuilder::build()`
is what returns the `WebView`, and the handler is registered before that call,
so at the point the handler closure is constructed there is no `WebView`
value yet for it to hold. `EventLoopProxy::send_event` and `StoreHandle`'s
methods both satisfy `Fn` on `&self`, which is why the handler posts a
`DuetEvent::WebviewScript` and lets the event loop — which does own the
`WebView` — evaluate it on a later turn instead.

### F10 — A structural discovery about depth guards

`duet_webview::decode` (`crates/duet-webview/src/lib.rs:60-73`) calls
`serde_json::from_str` before any of the text reaches
`duet_protocol::decode_request`, and therefore before it reaches
`duet_codec::decode_value` (`crates/duet-codec/src/value.rs:161`), which is
itself recursive over nested tagged values. Reading `serde_json` 1.0.151's
own source confirms the number already baked into the test's comment: a
freshly-constructed `serde_json::Deserializer` (`serde_json-1.0.151/src/de.rs:63`)
sets `remaining_depth: 128` — exactly 128, not "about" 128 — and rejects
input nested deeper than that on its own, before any `Value` type is ever
constructed. The consequence: **`duet_codec`'s recursive tagged-value decoder
is unreachable from guest text**, because the JSON parser sitting in front of
it is the depth guard. The test `hostile_guest_input_cannot_panic_hang_or_echo_unbounded_text`
(`crates/duet-webview/src/lib.rs:263-358`) makes this concrete: a `Value`
nested 5,000 deep inside a real `set` request (line 305-312) looks like it
targets `duet_codec`'s decoder specifically, but it hits `serde_json`'s guard
first, at the same ~128-level threshold as 200,000 raw `[` characters with no
structure at all. If a future change ever swaps in a parser without its own
recursion limit, this guard disappears and `duet_codec::decode_value`'s own
recursion becomes reachable for the first time — worth flagging for whoever
makes that change, since nothing today tests the codec's recursion directly.

### F11 — Hostile input is bounded, with exact numbers reproduced

Ran `cargo test -p duet-webview hostile_guest_input_cannot_panic_hang_or_echo_unbounded_text`
with a temporary `eprintln!` added after each of the two size-sensitive
assertions (removed afterward; `git diff` on `crates/duet-webview/src/lib.rs`
is empty again). Measured, not estimated:

| Hostile input | Result | Reply |
|---|---|---|
| 200,000 unclosed `[` | `failed` (serde_json's recursion guard, F10) | small, fixed-shape |
| 5,000-deep nested tagged `Value` inside a real `set` | `failed` (same guard, F10) | small, fixed-shape |
| 1 MB `path` string | `failed` | **exactly 100 bytes**: `{"id":"1","kind":"failed","message":"invalid path: unexpected character ']' at byte offset 1000000"}` |
| 1 MB bogus value `t` tag | `failed` | **exactly 111 bytes**: `{"id":"1","kind":"failed","message":"unknown type tag \"zzz...zzz…\""}`, the tag truncated to `duet_codec::error::MAX_ECHO_CHARS = 48` chars (`crates/duet-codec/src/error.rs:8-11`) |
| lone UTF-16 surrogate (`\ud800`) in a string | `failed`, no panic | small |
| raw control character (`\u{0007}`) in a string | `failed`, no panic | small |

No panic, no hang, no reply that scales with the size of the offending
input, across all six cases — the guest cannot turn a megabyte of garbage
into a megabyte of host-produced text (e.g. downstream in a log line). This
is proven against `handle_ipc_text` called directly, not through a real
`wry` IPC channel — see F13 on what that does and does not establish.

### F12 — A crate was extracted mid-phase; the lesson is about CI, not about code cleanliness

`duet-webview`'s `Cargo.toml` depends only on `duet-core`, `duet-runtime`,
`duet-protocol`, and `serde_json` — no `tao`, `wry`, or `objc2`, confirmed by
reading it directly. Nothing in `handle_ipc_text`, `response_script`,
`push_script`, or `bootstrap::BOOTSTRAP_HTML` touches a platform API. That
logic started inside `duet-backend-macos` (commit `3ef46e8`) alongside the
first IPC routing work, which meant it lived in the one crate this
workspace's CI cannot build at all: `.github/workflows/duet.yml` runs a
single job on `runs-on: ubuntu-latest` (line 14) and passes
`--exclude duet-backend-macos` to every workspace-wide `clippy`, `doc`,
`llvm-cov`, and `test` invocation (lines 33, 35, 39, 41) — the same exclusion
F2b-3's CI section documents and justifies for the platform-specific code.
While the routing logic sat inside `duet-backend-macos`, that exclusion also
hid it: its tests never ran in CI, and its coverage was never counted toward
the 90% gate, even though nothing about it required a window server, a
Flutter framework, or an Objective-C runtime. Commit `a39fc97` moved it out
into `duet-webview`, which CI now builds and tests like any other crate (see
the workspace verification below — `duet_webview`'s 9 unit tests run in the
same `cargo test --workspace --exclude duet-backend-macos` invocation as
everything else).

**Lesson for later phases:** platform-free logic written inside a
platform-gated crate is invisible to CI and will get duplicated per platform
if left there — Phase 5's Windows and Linux backends need this exact IPC
routing and bootstrap script, and without this extraction each would have
either reimplemented it or reached across into `duet-backend-macos`'s
macOS-only crate to borrow it. The fix was mechanical (move the code, no
logic changes — `git diff --stat main -- crates/duet-webview` shows only new
files, not modifications elsewhere), which is itself the point: the platform
boundary should be drawn by dependencies, not by which crate happened to be
open when the code was written.

### F13 — What could not be verified here, stated plainly

- **Nothing was seen on a display.** Unchanged from every prior phase: no
  reachable on-screen WindowServer for spawned processes. The window and
  `WKWebView` are created and JavaScript genuinely runs (F6), but no human
  observed any of it.
- **Real mouse/keyboard input to the webview is unproven, in either
  direction.** Spike B found synthetic input events reach a Flutter view but
  *not* a `WKWebView`, and left that unexplained. Nothing in
  `examples/webview_state.rs` touches input at all — this phase neither
  confirms nor contradicts Spike B's finding.
- **Hostile input has not been driven through the real `wry` IPC channel.**
  F11's totality proof calls `handle_ipc_text` directly, the same way the
  unit tests do. A guest actually sending malformed text over
  `window.ipc.postMessage` and having it arrive at `WebviewSurface`'s
  `with_ipc_handler` closure (`src/webview.rs:48-61`) intact is untested —
  plausible given `Request::body()` is just the string `wry` received, but
  not observed.
- **Subscriber isolation between two *live* guests is only proven
  structurally.** `the_handler_ignores_any_subscriber_named_on_the_wire`
  (`crates/duet-webview/src/lib.rs:184-230`) pins that `handle_ipc_text` uses
  the host-supplied `SubscriberId` argument and ignores any `"subscriber"`
  field on the wire, by calling the handler twice in sequence with two
  different subscriber ids. That is a real property of the function, but no
  run in this phase — including `webview_state.rs`, which creates exactly
  one surface — has had a webview and a Flutter engine subscribing
  simultaneously against one shared store to observe cross-guest isolation
  in the running system, not just in the function signature.
- **`duet-webview` sits at 89.78% line coverage on its own, below the
  workspace's 90% gate.** Measured directly from
  `cargo llvm-cov --workspace --exclude duet-backend-macos --locked --fail-under-lines 90`'s
  per-file table: `duet-webview/src/lib.rs` is 194 lines, 18 missed
  (90.72%); `duet-webview/src/bootstrap.rs` is 31 lines, 5 missed (83.87%).
  Combined: 225 lines, 23 missed = **89.78%**. The gate still passes overall
  only because `--fail-under-lines 90` is evaluated workspace-wide: total
  lines across all seven crates are 5,592 with 235 missed, 95.80%. This is
  reported as a real, currently-uncovered gap in `duet-webview` specifically
  — not hidden by the workspace total, and not something this phase closed.

---

### Workspace verification run for this phase

All commands below were run against this branch's actual `HEAD`
(`d15a4ff`), after F8's sabotage edit and F11's `eprintln!` instrumentation
were both reverted (`git status --short` shows no changes to any tracked
file under `crates/`).

```
cargo test --workspace --exclude duet-backend-macos --locked -- --test-threads=1
  # 355 tests total (353 unit/integration + 2 doctests), all passed, 0 failed
  # includes duet_webview: 9/9 (bootstrap: 2, lib: 7)

cargo llvm-cov --workspace --exclude duet-backend-macos --locked --fail-under-lines 90
  # TOTAL: 95.80% lines (5592 lines / 235 missed) -- gate passes (workspace-wide)
  # duet-webview/src/lib.rs:       90.72% lines (194/18 missed)
  # duet-webview/src/bootstrap.rs: 83.72% regions, 83.87% lines (31/5 missed)
  # duet-webview combined:         89.78% lines -- below the 90% gate on its own, see F13

cargo clippy --workspace --exclude duet-backend-macos --all-targets --locked -- -D warnings
  # clean, no warnings

cargo clippy -p duet-backend-macos --all-targets --locked -- -D warnings
  # clean, no warnings

cargo doc --workspace --exclude duet-backend-macos --no-deps --locked
  # clean; generates docs for duet-webview along with the other five crates

cargo fmt --all -- --check
  # clean

cargo test -p duet-backend-macos
  # 2 passed, 1 ignored (tao's EventLoop must be built on the main thread,
  # which the test harness does not provide -- same pre-existing constraint
  # F5's sink.rs test documents), 0 failed

git diff --stat main -- crates/duet-core crates/duet-runtime crates/duet-codec crates/duet-supervisor crates/duet-host crates/duet-protocol
  # (no output -- all six untouched by this phase)
```

Every command above passed or produced the expected clean/empty output; none
were re-run after a fix, and nothing here was adjusted to make a command
pass.
