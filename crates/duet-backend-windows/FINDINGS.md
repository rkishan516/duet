# duet-backend-windows — findings and recorded evidence

The Windows sibling of `crates/duet-backend-macos/FINDINGS.md`, holding what
was measured and observed **on real hardware** while building this crate.
Numbered WB1… so citations cannot collide with the macOS F-numbers or the
spike's W-F numbers (`spikes/spike-b-windows/FINDINGS.md`, which holds the
pre-crate spike findings this crate was designed from).

Environment for every measurement below: Windows 11 Pro 10.0.26200 x64, a
real display session, Flutter 3.47.0-1.0.pre-237 (master), Rust 1.92 MSVC,
`tao` 0.36.0, `wry` 0.56, WebView2 Runtime 151.0.4129.72. Guest fixtures
built with `flutter build windows --debug` (debug/JIT only, as everywhere in
this project).

---

## The seven examples: observed passes

docs/10-porting.md §7 sets the bar — an example is done when it prints
`ALL PASS` on real hardware and the output is pasted. All seven were run on
this machine, serialized, windows genuinely on screen. Final lines, verbatim:

| Example | Observed result |
|---|---|
| `webview_state` | `ALL PASS: a JavaScript guest and Rust share one store over real wry IPC` |
| `webview_commands` | `ALL PASS: a JavaScript guest invoked real host commands over real wry IPC` |
| `flutter_state` | `ALL PASS: a Dart guest and Rust share one store over a real Flutter platform channel` |
| `flutter_commands` | `ALL PASS: a Dart guest invoked real host commands over a real Flutter platform channel` |
| `two_guests` | `ALL PASS: two live guests share one store and neither can disturb the other` (all 12 checks) |
| `lifecycle` | both `PASS` lines — see WB2 for the numbers |
| `hot_reload` | `ALL PASS: a real Flutter engine hot-reloaded a real edit, and the Duet store kept its contents` — see WB5 |

Highlights worth pinning:

- **f64 bit-exactness (macOS F16, re-proven here):** all five probes
  round-tripped bit-for-bit over the real Windows channel, including
  `0x3fd3333333333334` (0.1+0.2), the subnormal `5e-324`
  (`0x0000000000000001`) and `-0.0` (`0x8000000000000000`).
- **Hostile input stayed bounded:** the 1 MB parseable path was answered with
  a 142-byte `Failed(id=9)`; the 1 MiB+1 payload was refused by the inbound
  cap with `Failed(id=0)` at 84 bytes.
- **Isolation held under attack:** the webview's hostile unsubscribe sweep
  (ids 0..=10) was answered 11/11, cancelled only its own subscriptions, and
  the Dart guest kept receiving its pushes — including after the webview was
  torn down entirely.

**Not verified here** (same honesty as the macOS record): real human
mouse/keyboard input — these were autonomous runs with nobody at the desk.
The spike's W-F7 covers what posted synthetic messages can and cannot show.
Release/AOT builds remain unexercised on every platform.

## WB1 — Engine boot paths must be absolutized: the engine resolves them against the EXE

`flutter_state`'s first run failed inside `FlutterDesktopEngineRun` with
`'FlutterEngineInitialize' returned 'kInvalidArguments'. Not running in AOT
mode but could not resolve the kernel binary` — while the example's own
existence checks on the same relative data-dir path had just passed. The
engine resolves relative `assets_path`/`icu_data_path` against the directory
containing the executable (`target/debug/examples/`), not the process working
directory. `FlutterEngine::boot` therefore canonicalizes the data dir before
handing it over, for every caller; the `\\?\` verbatim form `canonicalize`
produces is accepted (the spike booted from one).

## WB2 — Lifecycle RSS on this machine: teardown 73.6%, detach −2.3%

`examples/lifecycle`, spike_app fixture, real window on screen:

```text
process start, no surface registered                                16084 kB
renderer started, view attached (Readiness::Ready)                 179116 kB
after rasterizing the attached view                                222496 kB
view detached, suspending (engine still alive)                     226052 kB
torn down (engine shut down, if the grace period truly elapsed)     71552 kB

teardown reclaimed 73.6% of what the engine cost (floor 50%)
detaching accounted for -2.3% of that (ceiling 20%)
```

The macOS thresholds (≥50% reclaim, ≤20% by-detach) hold on Windows without
adjustment — comfortably: macOS measured 60.3–68.4% reclaim. Detach-as-parking
reclaims *nothing* (slightly negative here), which is exactly the design
claim: on Windows the controller owns the engine (W-F1), so what
`destroy_renderer` destroys — `FlutterDesktopViewControllerDestroy` — is what
reclaims memory. No `Could not create the embedder backing store` storm
appeared at any point with the F1 lifecycle sends in place.

## WB3 — flutter_windows.dll statically links its CRT; engine switches need /DELAYLOAD

The `hot_reload` example sets `FLUTTER_ENGINE_SWITCHES` /
`FLUTTER_ENGINE_SWITCH_n` (vm-service-port, disable-service-auth-codes) in
`main`, and the engine reads them with `std::getenv`. On the first run the
switches were silently invisible: the VM service came up on a random port
**with** an auth code, and the reload driver's connect was refused.

Two facts compose into the failure:

1. `flutter_windows.dll` links its CRT **statically** — `dumpbin /DEPENDENTS`
   lists no `ucrtbase.dll` and no `api-ms-win-crt-*` at all — and a static
   CRT snapshots the process environment once, at DLL load.
2. Eagerly linked, that load happens at process start, before `main`. Nothing
   a program sets afterwards — `SetEnvironmentVariableW` or even the shared
   UCRT's `_putenv_s` (tried, and measured not to help) — can reach it.

The fix is in `build.rs`: every example is linked with
`/DELAYLOAD:flutter_windows.dll` (+ `delayimp.lib`), so the DLL — and its
environment snapshot — loads on the *first engine call*, after a driver has
set its switches. macOS never had this hazard: `setenv` updates the one
`environ` every reader in the process shares. Library consumers building
their own executables do not inherit the flags and must arrange the same
thing themselves if they use engine switches.

## WB4 — duet-dev had three real Windows bugs, caught by the hot_reload run and now under test

All three were invisible to CI (ubuntu-only) and pre-existing:

1. **`frontend_server::file_uri`** glued `file://` to `Path::display()`,
   producing `file://C:\...` — drive letter where a URI host goes, plus raw
   backslashes. Every `reloadSources` on Windows was **declined with no
   stated reason**. Now emits `file:///C:/...`; pinned by a test on every
   platform.
2. **`FlutterSdk::locate`** looked for `dartaotruntime` with no extension;
   the Windows Dart SDK ships `dartaotruntime.exe`, so a fully-populated real
   SDK could never resolve. Now resolves the platform's name.
3. **`packages::resolve`** stripped `file://` from a rootUri and kept the
   rest, so `file:///C:/x` became `/C:/x` — not an absolute Windows path.
   Now drops the drive URI's leading slash.

Additionally two duet-dev *tests* were unportable rather than wrong
(separator-sensitive expectations; a relative-path helper that assumed the
temp directory shares a volume with the working directory — on this machine
the repo is on `D:` and temp on `C:`, and no relative path exists between
drives). `cargo test -p duet-dev`: 145/145 on this machine.

## WB5 — Hot reload works end to end on Windows, and it is fast

With WB3 and WB4 fixed, `examples/hot_reload` on real hardware:

```text
PASS: every reload was applied - 10/10 reload(s) reported success
PASS: each reload was incremental, not a full reload - libraries received vs kept: [(Some(4), Some(546)), ...]
PASS: the Dart-side change took effect in a rendered frame - 10/10
PASS: the Duet store's contents survived every reload - hostWitness Int(4242) intact
PASS: it was a hot reload, not a restart - nonce identical across all 10
PASS: the guest's own frame counter never reset - frames [3, 5, 7, ... 21]

LATENCY (fs::write -> the new marker readable from the Rust store, having
been built into a rendered frame):
  n=10 min=49.7ms median=57.1ms max=94.4ms mean=59.9ms
  the 500 ms parity bar: MET (max sample 94.4ms)
```

`FlutterDesktopEngineRun` honoring the environment switches (once visible,
WB3) also answers the porting brief's F26 question for Windows:
`vm-service-port` and `disable-service-auth-codes` both took effect — the VM
service came up on exactly the requested port with a bare `/ws` URL.

## WB6 — What the unit tests can now cover that macOS's cannot

`tao` on Windows can build an event loop off the main thread
(`EventLoopBuilderExtWindows::with_any_thread`), so the
closed-loop-reports-`Closed` `ProxySink` test runs un-`#[ignore]`d under the
ordinary `cargo test` harness — the first time that contract is exercised by
a test runner instead of by hand. 17/17 crate tests pass with no display
dependency, which is what lets the `windows` CI job in
`.github/workflows/duet.yml` actually run them.
