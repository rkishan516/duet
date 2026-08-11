# duet-backend-linux — findings and recorded evidence

The Linux sibling of `crates/duet-backend-macos/FINDINGS.md` and
`crates/duet-backend-windows/FINDINGS.md`, holding what was measured and
observed **on real hardware** while building this crate. Numbered LB1… so
citations cannot collide with the macOS F-numbers, the Windows WB-numbers,
or the spike's L-F numbers (`spikes/spike-b-linux/FINDINGS.md`, which holds
the pre-crate spike findings this crate was designed from).

Environment for every measurement below: WSL2 (Ubuntu 26.04 LTS, kernel
6.6.87.2-microsoft-standard-WSL2) under Windows 11 Pro 10.0.26200 x64, WSLg
display (X11 path — `GDK_BACKEND=x11`), Flutter master 7870373605 (the
pinned toolchain commit), Rust 1.97.1, GTK 3.24.52, WebKitGTK 2.52.3
(`libwebkit2gtk-4.1-0`), `tao` 0.36.0, `wry` 0.56. WSLg exposes no usable
GL ("Could not determine GL version"), so every run uses
`FLUTTER_LINUX_RENDERER=software` with Impeller disabled by the crate
(L-F4); guest fixtures built with `flutter build linux --debug` (debug/JIT
only, as everywhere in this project).

---

## The seven examples: observed passes

docs/10-porting.md §7 sets the bar — an example is done when it prints
`ALL PASS` on real hardware and the output is pasted. All seven were run on
this machine, serialized, windows genuinely mapped on the WSLg desktop.
Final lines, verbatim:

| Example | Observed result |
|---|---|
| `webview_state` | `ALL PASS: a JavaScript guest and Rust share one store over real wry IPC` |
| `webview_commands` | `ALL PASS: a JavaScript guest invoked real host commands over real wry IPC` |
| `flutter_state` | `ALL PASS: a Dart guest and Rust share one store over a real Flutter platform channel` |
| `flutter_commands` | `ALL PASS: a Dart guest invoked real host commands over a real Flutter platform channel` |
| `two_guests` | `ALL PASS: two live guests share one store and neither can disturb the other` (all 12 checks) |
| `lifecycle` | both `PASS` lines — see LB3 for the numbers |
| `hot_reload` | `ALL PASS: a real Flutter engine hot-reloaded a real edit, and the Duet store kept its contents` — see LB4 |

Highlights worth pinning:

- **f64 bit-exactness (macOS F16, Windows re-proven, now Linux):** all five
  probe doubles round-tripped bit-exactly over the real GTK platform
  channel.
- **Hostile input stayed bounded:** all 5 hostile payloads in
  `flutter_state` were answered with bounded failed replies; the reply
  never scaled with the request.
- **Isolation held under attack:** the webview's hostile unsubscribe sweep
  (ids 0..=10) was answered 11/11, cancelled only its own subscriptions
  (its later `jsOnly` write never arrived: push count stayed 2), and the
  Dart guest kept receiving its pushes — including after the webview was
  torn down entirely (`dartOnly = Int(11)`, its 4th push).
- **Both guests reached the host within 1.26 s** of process start in
  `two_guests` — after LB1's fix; before it the same milestone took 16.7 s
  and the sequence never finished.

**Not verified here** (same honesty as the other two records): real human
mouse/keyboard input — these were autonomous runs with nobody at the desk;
synthetic input has not been measured on this platform at all. The
`lifecycle` PNG capture (2,710 bytes via `gdk_pixbuf_get_from_window`) was
written but its pixel content is not asserted by the run. GL-capable
desktops are unexercised — everything here is the software rasterizer.
Release/AOT builds remain unexercised on every platform.

## LB1 — tao's GTK loop parks `WaitUntil` with no timer: quiet sessions stall turns for 10+ seconds

The first `two_guests` run timed out mid-sequence (45 s deadline, stage
`SettleAfterAttack`) with every reached check green, and the first
`lifecycle` run took 30 s for a 3-second script — its `WaitUntil(now)` turn
arrived 16 s late, and a `+700 ms` turn 12 s late. The webview examples
looked like the guest was slow (first push observed at 12.9 s) and the
numbers initially pointed at WebKitGTK timer throttling; both classic
WebKit-under-WSL overrides were tried and both measured *worse*
(`WEBKIT_DISABLE_DMABUF_RENDERER=1` and `WEBKIT_DISABLE_COMPOSITING_MODE=1`
each pushed even the guest bootstrap from 0.9 s to 10.7 s and timed the run
out), which is what forced a second look at the host side.

The actual mechanism is in `tao` 0.36's GTK event loop
(`platform_impl/linux/event_loop.rs`): a pending
`ControlFlow::WaitUntil(deadline)` with an empty event queue sets
`blocking = true` and enters `gtk_main_iteration_do(true)` — a fully
blocking iteration with **no timer source armed for the deadline**. The
deadline is only re-checked after *something else* wakes the main context.
On a busy desktop something always does; under WSLg with no mapped window
rendering frames (a headless Flutter guest, a settled webview page, a
window just hidden by detach) the next stray X11 event can be 10–16 s away.
The correlation was exact: `lifecycle`'s turns were on time precisely while
spike_app's `Ticker` was pushing frames into a mapped window, and stalled
before the first map and after the detach's hide.

The fix in every tao-loop example is a **metronome thread**: a proxy send
is a glib channel source and *does* wake the context (the same mechanism
the spike's 722/722 closed-loop run rode), so a thread posting
`DuetEvent::Tick` every 50 ms keeps the deadline checks honest, and the
Tick events themselves are no-ops. Measured effect: `webview_state`'s first
push 12.86 s → 1.44 s, `two_guests` both-guests-up 16.73 s → 1.26 s and
the full 12-check sequence from wedged-past-deadline to done in under 20 s,
`lifecycle` wall time 30.1 s → 2.73 s.

The library itself does not need the metronome: everything `ProxySink`
delivers arrives as a proxy send and wakes the loop on its own. But any
*driver* that relies on tao `WaitUntil` deadlines while its session is
quiet inherits this hazard, on WSLg or any sparse-event X session.

## LB2 — Dropping the last *owned* engine reference does not shut the engine down; dispose must be forced

The first `lifecycle` run failed its own floor assertion exactly as
designed: teardown reclaimed **5.4%** of the engine's cost (10,924 kB of
203,404 kB). Instrumenting `FlutterEngine::shutdown` showed the engine's
GObject `ref_count` still at **2** after `gtk_widget_destroy` of the
implicit view — one embedder-internal reference outlives the view, so the
crate's own unref leaves the count at 1, `FlEngine`'s dispose never runs,
and `FlutterEngineShutdown` (which lives in that dispose) never stops Dart.
The design note carried from the spike — "reclaim is the last reference
drop" — was wrong in a way only an RSS measurement could catch.

The fix: `FlutterEngine::shutdown` now runs `g_object_run_dispose` on the
engine — the GObject idiom for breaking exactly this kind of internal
cycle, and what `gtk_widget_destroy` itself does to widgets — then drops
its own reference; remaining holders finalize the husk whenever they drop.
With the fix, the same run reclaims **56.5%** (LB3). Two consequences are
now part of the crate's contract: `destroy_renderer` is what stops Dart
(not a later surface drop), and `Messenger` holds its own reference on the
`FlBinaryMessenger` object as well as the engine, so a surface outliving
`destroy_renderer` sends into a live messenger whose internal *weak*
engine reference makes the send a safe no-op instead of a dangling call.

## LB3 — Lifecycle RSS on this machine: teardown 56.5%, detach 1.2%

`examples/lifecycle`, spike_app fixture, real window mapped on the WSLg
desktop, with LB1's metronome and LB2's forced dispose in place:

```text
process start, no surface registered                                30976 kB
renderer started, view attached (Readiness::Ready)                 231636 kB
after rasterizing the attached view                                299220 kB
view detached, suspending (engine still alive)                     297448 kB
torn down (engine shut down, if the grace period truly elapsed)    146788 kB

teardown reclaimed 56.5% of what the engine cost (floor 50%)
detaching accounted for 1.2% of that (ceiling 20%)
```

The macOS thresholds (≥50% reclaim, ≤20% by-detach) hold on Linux without
adjustment — macOS measured 60.6–68.3%, Windows 73.6%. Detach-as-hide
reclaims essentially nothing (1.2%), which is the design claim: on Linux
the view never leaves its window (L-F3), so what `destroy_renderer` forces
— `FlEngine`'s dispose — is what reclaims memory. No
`Could not create the embedder backing store` storm appeared at any point
with the F1 lifecycle sends in place, completing the trilogy: the storm
that hung macOS has now failed to reproduce on all three platforms with
the sends present.

## LB4 — Hot reload works end to end on Linux, with no delay-load analog needed

Windows needed `/DELAYLOAD` (WB3) because `flutter_windows.dll`'s static
CRT snapshots the environment at DLL load. Linux has no such hazard:
`libflutter_linux_gtk.so` reads switches with `std::getenv` against the
one glibc `environ` every reader shares, so `examples/hot_reload` sets
`FLUTTER_ENGINE_SWITCHES` / `FLUTTER_ENGINE_SWITCH_n` (vm-service-port,
disable-service-auth-codes) in `main` before boot and they simply took
effect — the VM service came up on exactly the requested port with a bare
`/ws` URL. `duet-dev`'s POSIX paths needed no changes (the WB4 fixes were
already platform-symmetric). On real hardware:

```text
PASS: every reload was applied - 10/10 reload(s) reported success
PASS: each reload was incremental, not a full reload - libraries received vs kept: [(Some(4), Some(546)), ...]
PASS: the Dart-side change took effect in a rendered frame - 10/10
PASS: the Duet store's contents survived every reload - hostWitness Int(4242) intact
PASS: it was a hot reload, not a restart - nonce identical across all 10
PASS: the guest's own frame counter never reset - frames [3, 5, 7, ... 21]

LATENCY (fs::write -> the new marker readable from the Rust store, having
been built into a rendered frame):
  n=10 min=51.2ms median=60.2ms max=83.3ms mean=61.4ms
  the 500 ms parity bar: MET (max sample 83.3ms)
```

Median 60.2 ms against Windows' 57.1 ms and the macOS reference's ~40 ms —
the software rasterizer under WSLg costs almost nothing on this path.

## LB5 — What the unit tests cover, at parity with Windows

`tao` on Linux offers `EventLoopBuilderExtUnix::with_any_thread`, so the
closed-loop-reports-`Closed` `ProxySink` test runs un-`#[ignore]`d under
the ordinary `cargo test` harness, exactly as on Windows (WB6) and unlike
macOS. 19/19 crate tests pass with no display dependency — the platform-free
handler logic, the 1 MiB size caps, the lossy-UTF-8 decode, the subscriber
filter, the const failure replies, and the backend bookkeeping (deferred
close, destroy idempotence, headless boot reaching the assets check). The
planned Linux CI job can run them all headless.
