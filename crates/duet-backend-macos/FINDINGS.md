# Phase 2b-3 — `duet-backend-macos`: findings from running it for real

**Overall verdict: the reclaim mechanism itself works — Spike A already measured
that — but this phase's own real run could not reach it.** `examples/lifecycle.rs`
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
