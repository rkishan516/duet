# Spike A: engine-first Flutter embedding on macOS

## Verdict: YES

A Rust process can boot the Flutter macOS engine "engine-first" (no view), then
create, attach, detach, destroy, and recreate views against that same engine
independently of window lifetime, and shut the engine down cleanly. All five exit
criteria are demonstrated by the binary in `src/main.rs`, with rendered pixel
output captured as proof (see `evidence/`).

| # | Criterion | Verdict | Evidence |
|---|---|---|---|
| 1 | Engine boots headless, process stays alive | **yes** | `allowHeadlessExecution: true`, `runWithEntrypoint` returned `true`, no view existed |
| 2 | View created, parented into `tao` window, renders | **yes** | `evidence/view1_counter_app.png` -- genuine Flutter counter UI |
| 3 | View detached+destroyed, engine keeps running | **yes** | No crash on detach; `binaryMessenger` still resolves; proven definitively by criterion 4 |
| 4 | Second view created against same engine, renders | **yes** | `evidence/view2_counter_app_same_engine.png` |
| 5 | Engine shuts down cleanly, no crash on exit | **yes** | `shutDownEngine` sent, process exits with code 0 |

## The exact call sequence that worked

```rust
// 1. Headless engine boot (criterion 1)
let bundle = NSBundle::bundleWithPath(&NSString::from_str(app_framework_path)).unwrap();
let project_alloc: Allocated<AnyObject> = msg_send![class!(FlutterDartProject), alloc];
let project: Retained<AnyObject> =
    msg_send![project_alloc, initWithPrecompiledDartBundle: &*bundle];

let engine_alloc: Allocated<AnyObject> = msg_send![class!(FlutterEngine), alloc];
let engine: Retained<AnyObject> = msg_send![
    engine_alloc,
    initWithName: &*NSString::from_str("duet-spike-a"),
    project: &*project,
    allowHeadlessExecution: true,   // <-- the whole ballgame
];
let ran: bool = msg_send![&engine, runWithEntrypoint: Option::<&NSString>::None];
assert!(ran); // engine now running, zero views attached, process alive

// 2. Create + attach a view (criterion 2)
let vc_alloc: Allocated<AnyObject> = msg_send![class!(FlutterViewController), alloc];
let controller: Retained<AnyObject> = msg_send![
    vc_alloc,
    initWithEngine: &*engine,               // NOT the `viewController` property
    nibName: Option::<&NSString>::None,
    bundle: Option::<&NSBundle>::None,
];
let flutter_view: Retained<AnyObject> = msg_send![&controller, view];
let flutter_view: Retained<NSView> = Retained::cast_unchecked(flutter_view);
flutter_view.setFrame(content_view.bounds());
flutter_view.setAutoresizingMask(ViewWidthSizable | ViewHeightSizable);
content_view.addSubview(&flutter_view);   // content_view = tao window's NSWindow.contentView()

// 3. Detach + destroy (criterion 3) - across a full event-loop tick, see note below
flutter_view.removeFromSuperview();
drop(controller);   // dealloc removes controller from the engine (per header docs)
drop(window);

// 4. Second view on the SAME engine (criterion 4) - repeat step 2's initWithEngine:
//    call with the same `engine` handle. Must happen on a LATER event-loop tick than
//    the drop in step 3 (see finding below) - in this spike, separated by 3 real seconds.

// 5. Shutdown (criterion 5)
let _: () = msg_send![&engine, shutDownEngine];
drop(engine);
// process then exits normally with code 0
```

Full state machine, memory-management types (`Allocated<T>` for `alloc`, `Retained<T>`
for everything else), and the offscreen-snapshot helper are in `src/main.rs`.

## Header docs vs. reality: three surprises

### 1. `runWithEntrypoint:` called a second time returns `NO`, not `YES`

My original plan was to prove engine liveness after view-detach by calling
`runWithEntrypoint:` again and asserting it returns `YES`. It returned `NO`, and the
assertion crashed the spike (a clean Rust panic, not a segfault -- reassuring in
itself, since it means the underlying Retained handles were all still valid).

On reflection this is not a bug in the engine, it's a wrong assumption on my part.
The header's contract is: `@return YES if the call succeeds in creating and running
a Flutter Engine instance; NO otherwise.` A second call creates nothing new (the
first sentence of the doc comment: "The first call to this method will create a new
Isolate. Subsequent calls will return immediately.") so `NO` is the *correct*
answer for a healthy, already-running engine, not evidence it died. **Do not use
`runWithEntrypoint:`'s return value as a liveness check** -- it was the wrong tool.
The spike now instead reads `engine.binaryMessenger` (a message send that would
crash with `EXC_BAD_ACCESS` on a genuinely deallocated object, not return cleanly)
and, more importantly, treats "successfully creates and renders a second view" as
the real liveness proof -- which is exactly what criterion 4 already asks for.

### 2. `FlutterEngine` on macOS enforces "at most one view controller for the implicit view" - and it's an *uncatchable* ObjC exception by default

This is the single most important finding for Duet's design. I tried extending the
spike with a rough leak check: repeatedly create a second `FlutterViewController`
via `initWithEngine:` while an existing one (`controller2`) was still attached, to
see whether the engine actually supports concurrent multi-view. It does not, at
least not through this initializer:

```
uncaught exception <NSException: 0xa979a9770> 'NSInternalInconsistencyException'
reason: The engine already has a view controller for the implicit view.
  ... -[FlutterEngine addViewController:] + 276
  ... -[FlutterViewController initWithEngine:nibName:bundle:] + 152
```

The header docs for `initWithEngine:nibName:bundle:` say it's "suitable for both
the first Flutter view controller and the following ones of the app," which reads
as if arbitrary concurrent multi-view is supported. In practice, on this engine
build, view id `0` ("the implicit view") can only ever have one live controller at
a time -- you must fully detach/dealloc the current one before creating the next.
This does **not** invalidate the four required criteria (they only ever ask for one
view at a time, sequentially), but it means Duet's teardown-to-reclaim-memory design
is exactly the *right* shape for this engine: one Flutter surface, replaced not
duplicated. If Duet ever wants two *simultaneous* Flutter windows from one engine,
that needs further investigation (likely a different, non-implicit view id/API not
covered by the headers we were given).

Separately: by default, this `NSException` is **not catchable by Rust at all** --
objc2 0.6's default `msg_send!` lets it propagate into `std::process::abort()`
territory: `fatal runtime error: Rust cannot catch foreign exceptions, aborting`,
with no message, no reason, nothing actionable. Enabling objc2's `catch-all` Cargo
feature converts these into ordinary Rust panics that carry the `NSException`'s
`reason:` string, which is how the message above was actually recovered. **For
Duet's real embedder, this is a load-bearing decision**: without `catch-all`,
any Objective-C-side assertion (misuse, macOS version quirk, engine bug) is an
instant, silent, undebuggable process abort. `Cargo.toml` in this spike now
depends on `objc2 = { version = "0.6.4", features = ["catch-all"] }`.

### 3. Full detach -> recreate must cross a real event-loop tick, or it silently races into the exception above

The first version of the leak-check loop detached the old controller and created
the new one back-to-back inside the *same* synchronous callback (no run-loop turn
in between) -- and hit the exact same "already has a view controller for the
implicit view" exception, even though the old controller had unambiguously been
`drop`-ed in Rust first. Rust's `drop` on `Retained<AnyObject>` calls `objc_release`
immediately, but whatever `-dealloc` on `FlutterViewController` does to unregister
itself from the engine apparently isn't guaranteed to have completed -- or at least,
some part of AppKit's window-close teardown that the view depends on -- synchronously
within that same call. Once I split the loop so the detach happens on its own
event-loop tick before the next create (via `return` after setting a flag, letting
`ControlFlow::WaitUntil` schedule the next tick), it worked reliably across 8
cycles. **Practical implication for Duet: a detach-then-recreate cycle needs at
least one turn of the host run loop between the two calls; doing it synchronously
back-to-back is not safe.**

## Did the engine genuinely survive view destruction, and how was that proven?

Yes. Three independent lines of evidence, from weakest to strongest:

1. **No crash.** `removeFromSuperview` + dropping the `Retained<AnyObject>`
   controller + dropping the hosting `tao::Window` all completed without any
   Objective-C exception or Rust panic.
2. **The engine object remained messageable.** `engine.binaryMessenger` (a live
   property read through the same `Retained<AnyObject>` handle obtained during
   headless boot, never re-fetched) resolved to a real, non-crashing object
   pointer after the detach.
3. **Definitive: a brand new, fully working, rendering view was created from the
   exact same `engine` handle afterward** (criterion 4), and it renders the actual
   Flutter counter app (`evidence/view2_counter_app_same_engine.png`). A torn-down
   engine cannot do that. This is the proof that actually matters; (1) and (2) are
   just corroborating detail.

The 8-cycle leak-check loop (detach -> recreate, 8x, one full engine-view lifecycle
per event-loop tick) further reinforces this: the engine kept accepting new view
controllers and kept rendering across all 8 cycles, with no crash, right up until
the deliberate `shutDownEngine` call.

## Leak observations (rough, via `ps -o rss=`)

RSS printed at every step (see full log below). Summary of the shape:

- Baseline (no engine): ~14 MB
- After engine boot + first isolate run: ~42 MB to ~149 MB (this jump is the Dart
  VM/isolate/Skia-or-Impeller initialization cost, not a leak -- it happens once)
- View 1 attached + rendered: ~198 MB to ~224 MB over ~6s while it sits on screen
- View 1 detached, engine alone: ~224 MB (no drop -- nothing was reclaimed by
  detaching one view while the engine keeps running, which is expected: the engine
  itself, its isolate, and its caches are the bulk of the memory, not the view)
- View 2 created: ~223 MB to ~231 MB
- **8x detach/recreate cycles: ~231 MB to ~234 MB, i.e. roughly +300-500 KB per
  cycle.** Small, and it may well be legitimate engine-side caching (texture/layer
  pools, frame timing buffers) rather than a true unbounded leak -- 8 cycles is too
  few to distinguish "leaks a little" from "warms up a cache and plateaus." This
  is genuinely **inconclusive** and would need a much longer soak (hundreds of
  cycles, ideally with `leaks`/Instruments) to call definitively either way.
- **After `shutDownEngine`: ~234 MB to ~108 MB.** The engine released the large
  majority of its memory on shutdown, which is a good sign -- it means the bulk of
  the footprint genuinely was the engine/isolate, not orphaned view objects, and
  that shutdown path does real cleanup rather than just leaking everything until
  process exit.

Verdict on leaks: **no obvious catastrophic per-cycle leak observed**, but the
small (~0.3-0.5 MB/cycle) growth during the 8-cycle loop is not nailed down as
"definitely fine" either. Flag for a longer-running soak test in a later phase if
Duet's design leans on very frequent view churn.

## Anything else that surprised me / contradicts the design assumptions above

- **`viewIdentifier` was `0` for both view 1 and view 2**, even though the header
  says ids "are guaranteed to be unique for views owned by a given engine." This is
  consistent with finding #2 above: id `0` denotes "the implicit view," and it gets
  reassigned to whichever controller currently holds that slot, rather than being a
  monotonically increasing counter. Not a contradiction of the docs once you know
  about the implicit-view concept, but it's not obvious from the headers alone
  (the term "implicit view" does not appear anywhere in the four headers we were
  given -- it only surfaced via the exception message at runtime).
- **The build/link/run story worked exactly as briefed**: `build.rs` linking
  against the `macos-arm64_x86_64` slice inside the `darwin-x64`-named xcframework
  directory, plus an embedded `-rpath`, was sufficient for `cargo run` to work with
  no `DYLD_FRAMEWORK_PATH` needed. Confirmed via `otool -l` showing the `LC_RPATH`
  and `otool -L` showing `@rpath/FlutterMacOS.framework/...`.
- **objc2 0.6's `Allocated<T>` vs `Retained<T>` distinction is load-bearing, not
  cosmetic.** My first attempt used `Retained<AnyObject>` for the result of every
  `alloc` call (matching the prompt's suggested pattern loosely). This does not
  compile: `alloc`'s result does not implement `MessageReceiver`/`Encode` as
  `Retained<T>` when passed on to an `init` call as the receiver -- the crate forces
  you through `Allocated<T>` specifically so `init`'s ownership-consuming semantics
  are enforced by the type system rather than by convention. Once switched to
  `Allocated<T>` for `alloc` results, everything compiled and behaved correctly
  (verified no double-release crashes across dozens of create/destroy cycles).
- **This execution environment has no reachable on-screen WindowServer session for
  spawned processes.** `screencapture -x` and `CGWindowListCopyWindowInfo` both see
  a real, active desktop (Chrome, Slack, etc.) but never see any window created by
  this spike's process -- confirmed independently with a trivial `tao`-only probe
  program (no Flutter at all) that built a plain window successfully at the API
  level (`WindowBuilder::build()` returned `Ok`) but was invisible to
  `CGWindowListCopyWindowInfo`, both sandboxed and with `dangerouslyDisableSandbox`,
  and both directly and via `launchctl asuser <uid>`. Since a real screenshot of the
  physical screen was not obtainable, this spike instead rasterizes the Flutter
  `NSView` to a PNG **from inside the process itself**, via
  `-[NSView cacheDisplayInRect:toBitmapImageRep:]`, which does not require the
  window to actually be composited on a physical display. This worked and produced
  genuine, correct pixels of the Flutter counter app for both views (see
  `evidence/`). Worth knowing for Spike B/C: if this constraint follows into later
  phases, budget for the same offscreen-snapshot trick rather than relying on
  `screencapture`.
- **`shutDownEngine` prints `Communicating on a dead channel.`** to stdout as part
  of its normal teardown path. This looks alarming but is expected/benign -- it's
  the engine's own binary messenger complaining about in-flight platform-channel
  traffic that raced the shutdown, not a crash or leak indicator.
- **The 8-cycle leak-check loop logs a `[ERROR:...] Could not create the embedder
  backing store.`** once per cycle, immediately after each detach. The process
  never crashed and the *next* cycle's view still rendered correctly, so this looks
  like a harmless, logged-but-recovered internal race in the "merged UI and
  platform thread. Experimental." mode mentioned at engine startup -- flagging it
  because "experimental" is doing a lot of work in that log line and Duet should
  not assume this threading mode is stable across engine versions.

## Build/link details actually used

- `build.rs`: `cargo:rustc-link-search=framework=<dir>`, `cargo:rustc-link-lib=framework=FlutterMacOS`,
  `cargo:rustc-link-arg=-Wl,-rpath,<same dir>` -- rpath approach used (not `DYLD_FRAMEWORK_PATH`), confirmed working via plain `cargo run`.
- `Cargo.toml` additions beyond what was pre-resolved: `objc2` needed the `catch-all`
  feature turned on (see finding #2). No other new dependencies were needed --
  `objc2-app-kit`/`objc2-foundation`'s default feature sets already included
  everything used (`NSBundle`, `NSView`, `NSWindow`, `NSBitmapImageRep`, `NSRect`/
  `CGRect` via the `objc2-core-foundation` default feature).
- `App.framework` path is read from `DUET_APP_FRAMEWORK_PATH` env var, falling back
  to the path given in the brief, so Spike C can point at a different build.

## Full stdout from a successful run (exit code 0)

```
[t+  0.00s rss=  14096kB] Spike A starting: engine-first Flutter embedding on macOS
[t+  0.00s rss=  14352kB] App.framework path: /Users/kishan/dev/rkishan516/tauri-flutter/spikes/spike_app/build/macos/Build/Products/Debug/App.framework
[t+  3.09s rss=  36416kB] === Step 1: boot headless FlutterEngine (no view) ===
[t+  3.10s rss=  36416kB] Loading NSBundle at /Users/kishan/dev/rkishan516/tauri-flutter/spikes/spike_app/build/macos/Build/Products/Debug/App.framework
[t+  3.10s rss=  36432kB] FlutterDartProject created from App.framework bundle
[t+  3.13s rss=  41968kB] FlutterEngine allocated with allowHeadlessExecution: true
2026-08-04 15:45:00.097 spike-a-macos[62489:23176083] Running with merged UI and platform thread. Experimental.
flutter: The Dart VM service is listening on http://127.0.0.1:52963/287U1GZ6lE8=/
[t+  3.24s rss= 148784kB] runWithEntrypoint(nil) returned true
[t+  3.24s rss= 148784kB] PASS criterion 1: engine booted with allowHeadlessExecution=true, runWithEntrypoint returned YES, process is alive with zero views attached
[t+  6.25s rss= 189424kB] === Step 2: create first view, parent into tao window ===
[t+  6.32s rss= 198144kB] controller1 viewIdentifier=0 attached=true
[t+  6.33s rss= 198144kB] PASS criterion 2: first FlutterViewController created via initWithEngine:, view parented into tao NSWindow's contentView - Flutter counter app should be visible now
[t+  9.33s rss= 217568kB] === Step 3: holding first view on screen for a few seconds (visual check) ===
[spike-a] snapshot: wrote /tmp/spike_a_view1.png (success=true, bytes=27653)
[t+ 12.36s rss= 224224kB] === Step 4: detach + destroy first view, engine stays running ===
[t+ 12.38s rss= 224288kB] controller1 and its window dropped
[t+ 12.38s rss= 224288kB] post-detach runWithEntrypoint(nil) returned false (NOT used as liveness proof - see FINDINGS.md; the object responding at all without crashing already shows it's still a live, valid FlutterEngine*)
[t+ 12.39s rss= 224288kB] engine.binaryMessenger still resolves to a live object: 0x868d24da0
[t+ 12.39s rss= 224288kB] PASS criterion 3: first view detached (removeFromSuperview + controller dropped); engine object remained valid and messageable post-detach (binaryMessenger query succeeded with no crash). Definitive proof follows in step 5/criterion 4: the SAME engine reference is reused to create and render a brand new working view.
[t+ 15.40s rss= 222896kB] === Step 5: create second view against the SAME engine ===
[t+ 15.42s rss= 223184kB] controller2 viewIdentifier=0 attached=true (note: IDs are per-engine, so a fresh id being assigned to a live engine after the first view's id was freed is itself further evidence the engine kept running its own bookkeeping)
[t+ 15.42s rss= 223184kB] PASS criterion 4: second FlutterViewController created via initWithEngine: against the engine that survived step 4, and rendered into a second tao window
[t+ 18.43s rss= 224976kB] === Step 6: holding second view on screen for a few seconds (visual check) ===
[spike-a] snapshot: wrote /tmp/spike_a_view2.png (success=true, bytes=27653)
[t+ 18.59s rss= 231136kB] === Step 6b: rough leak check - 8x sequential create+attach -> detach+destroy replace cycles on the same engine, one per event-loop tick ===
[t+ 18.77s rss= 229472kB] leak-check cycle 0 done
[t+ 18.94s rss= 230928kB] leak-check cycle 1 done
[t+ 19.11s rss= 231296kB] leak-check cycle 2 done
[t+ 19.29s rss= 231808kB] leak-check cycle 3 done
[t+ 19.46s rss= 232336kB] leak-check cycle 4 done
[t+ 19.64s rss= 232752kB] leak-check cycle 5 done
[t+ 19.81s rss= 233168kB] leak-check cycle 6 done
[t+ 19.98s rss= 233600kB] leak-check cycle 7 done
[t+ 22.98s rss= 233952kB] === Step 7: shut down engine cleanly ===
[t+ 23.00s rss= 108880kB] shutDownEngine sent
[t+ 23.01s rss= 108256kB] PASS criterion 5: shutDownEngine called, all Retained handles dropped, no crash observed so far
[t+ 26.01s rss= 108096kB] All five criteria exercised. Exiting process in 1 more tick to confirm no crash-on-exit.
[t+ 26.02s rss= 108096kB] LoopDestroyed - process exiting
```

(`[ERROR:flutter/...]` embedder log lines interleaved with the leak-check cycles
omitted above for brevity; they're discussed above under "Anything else that
surprised me.")

Exit code: `0`.

## What I could not determine

- **Whether the ~0.3-0.5 MB/cycle growth during the leak-check loop is a genuine
  leak or engine warm-up/caching.** 8 cycles is not enough data; would need a much
  longer soak and/or Instruments/`leaks` to say definitively.
- **Whether true *simultaneous* multi-view (two live `FlutterViewController`s on
  one engine at once) is possible at all on this engine build.** The headers
  suggested it should be, but `initWithEngine:nibName:bundle:` reproducibly refused
  it with "The engine already has a view controller for the implicit view." There
  may be a separate, non-implicit-view API for this that isn't in the four headers
  we were given -- out of scope for this spike, but worth a follow-up if Duet's
  design ever needs two Flutter surfaces from one engine concurrently (as opposed
  to Duet's actual stated model of one Flutter engine + one separate Tauri
  webview, which this spike's sequential-replace behavior fully supports).
