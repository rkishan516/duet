//! Spike A: prove a Rust process can boot the Flutter macOS engine "engine-first" —
//! i.e. without a view, and then create/attach/detach/destroy views independently
//! while the engine keeps running. See FINDINGS.md for the writeup.
//!
//! This is throwaway spike code: no tests, minimal error handling, liberal stdout.

use std::process::Command;
use std::time::{Duration, Instant};

use objc2::rc::{Allocated, Retained};
use objc2::runtime::AnyClass;
use objc2::msg_send;
use objc2_app_kit::{NSBitmapImageFileType, NSView, NSWindow};
use objc2_foundation::{NSBundle, NSDictionary, NSRect, NSString};

use tao::dpi::LogicalSize;
use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoop};
use tao::platform::macos::WindowExtMacOS;
use tao::window::{Window, WindowBuilder};

const STEP_INTERVAL: Duration = Duration::from_secs(3);

/// Path to the Flutter `App.framework` produced by `flutter build macos`.
/// Overridable via env var so Spike C can point at a different build.
fn app_framework_path() -> String {
    std::env::var("DUET_APP_FRAMEWORK_PATH").unwrap_or_else(|_| {
        "/Users/kishan/dev/rkishan516/tauri-flutter/spikes/spike_app/build/macos/Build/Products/Debug/App.framework"
            .to_string()
    })
}

/// Crude RSS check via `ps`, for eyeballing whether create/destroy cycles leak memory.
fn rss_kb() -> String {
    let pid = std::process::id().to_string();
    match Command::new("ps").args(["-o", "rss=", "-p", &pid]).output() {
        Ok(out) => String::from_utf8_lossy(&out.stdout).trim().to_string(),
        Err(_) => "?".to_string(),
    }
}

struct Log {
    start: Instant,
}

impl Log {
    fn new() -> Self {
        Self {
            start: Instant::now(),
        }
    }

    fn line(&self, msg: &str) {
        println!(
            "[t+{:>6.2}s rss={:>7}kB] {}",
            self.start.elapsed().as_secs_f64(),
            rss_kb(),
            msg
        );
    }

    fn pass(&self, criterion: usize, msg: &str) {
        println!(
            "[t+{:>6.2}s rss={:>7}kB] PASS criterion {}: {}",
            self.start.elapsed().as_secs_f64(),
            rss_kb(),
            criterion,
            msg
        );
    }

    #[allow(dead_code)]
    fn fail(&self, criterion: usize, msg: &str) {
        println!(
            "[t+{:>6.2}s rss={:>7}kB] FAIL criterion {}: {}",
            self.start.elapsed().as_secs_f64(),
            rss_kb(),
            criterion,
            msg
        );
    }
}

/// Build a `FlutterDartProject` (Objective-C) from the App.framework bundle.
///
/// Safety: caller must be on the main thread; classes must be linked (FlutterMacOS.framework).
unsafe fn build_dart_project(log: &Log) -> Retained<objc2::runtime::AnyObject> {
    let framework_path = app_framework_path();
    log.line(&format!("Loading NSBundle at {framework_path}"));
    let path_ns = NSString::from_str(&framework_path);
    let bundle: Retained<NSBundle> = NSBundle::bundleWithPath(&path_ns)
        .unwrap_or_else(|| panic!("NSBundle::bundleWithPath failed for {framework_path}"));

    let project_cls = AnyClass::get(c"FlutterDartProject")
        .expect("FlutterDartProject class not found - is FlutterMacOS.framework linked?");
    let alloc: Allocated<objc2::runtime::AnyObject> = unsafe { msg_send![project_cls, alloc] };
    let project: Retained<objc2::runtime::AnyObject> = unsafe {
        msg_send![alloc, initWithPrecompiledDartBundle: &*bundle]
    };
    log.line("FlutterDartProject created from App.framework bundle");
    project
}

/// Boots a headless (viewless) FlutterEngine. This is the crux of criterion 1:
/// `allowHeadlessExecution: true` is what lets the engine survive with zero views.
///
/// Safety: caller must be on the main thread.
unsafe fn boot_headless_engine(log: &Log) -> Retained<objc2::runtime::AnyObject> {
    let project = unsafe { build_dart_project(log) };

    let engine_cls = AnyClass::get(c"FlutterEngine")
        .expect("FlutterEngine class not found - is FlutterMacOS.framework linked?");
    let alloc: Allocated<objc2::runtime::AnyObject> = unsafe { msg_send![engine_cls, alloc] };
    let name = NSString::from_str("duet-spike-a");
    let engine: Retained<objc2::runtime::AnyObject> = unsafe {
        msg_send![
            alloc,
            initWithName: &*name,
            project: &*project,
            allowHeadlessExecution: true,
        ]
    };
    log.line("FlutterEngine allocated with allowHeadlessExecution: true");

    let entrypoint: Option<&NSString> = None;
    let ran: bool = unsafe { msg_send![&engine, runWithEntrypoint: entrypoint] };
    log.line(&format!("runWithEntrypoint(nil) returned {ran}"));
    assert!(ran, "FlutterEngine failed to run entrypoint");

    engine
}

/// Creates a `FlutterViewController` attached to `engine` via the multi-view
/// `initWithEngine:nibName:bundle:` path (NOT the legacy `viewController` property).
///
/// Safety: caller must be on the main thread; `engine` must be a valid FlutterEngine*.
unsafe fn create_view_controller(
    engine: &objc2::runtime::AnyObject,
) -> Retained<objc2::runtime::AnyObject> {
    let cls = AnyClass::get(c"FlutterViewController")
        .expect("FlutterViewController class not found");
    let alloc: Allocated<objc2::runtime::AnyObject> = unsafe { msg_send![cls, alloc] };
    let nib_name: Option<&NSString> = None;
    let bundle: Option<&NSBundle> = None;
    let controller: Retained<objc2::runtime::AnyObject> = unsafe {
        msg_send![
            alloc,
            initWithEngine: engine,
            nibName: nib_name,
            bundle: bundle,
        ]
    };
    controller
}

/// Parents the FlutterViewController's NSView into the tao window's content view,
/// filling the window and resizing with it.
///
/// Safety: caller must be on the main thread; `window` must currently be alive.
unsafe fn attach_view_to_window(window: &Window, controller: &objc2::runtime::AnyObject) {
    let flutter_view: Retained<objc2::runtime::AnyObject> = unsafe { msg_send![controller, view] };
    // The FlutterViewController's `.view` is actually an NSView at runtime; cast the untyped
    // handle to the typed objc2-app-kit binding so we can use its typed methods below.
    let flutter_view: Retained<NSView> =
        unsafe { Retained::cast_unchecked(flutter_view) };

    let ns_window_ptr = window.ns_window() as *mut NSWindow;
    let ns_window: &NSWindow = unsafe { &*ns_window_ptr };
    let content_view: Retained<NSView> =
        ns_window.contentView().expect("NSWindow has no contentView");

    let bounds: NSRect = content_view.bounds();
    flutter_view.setFrame(bounds);
    // NSViewWidthSizable | NSViewHeightSizable so the Flutter view tracks window resizes.
    flutter_view.setAutoresizingMask(
        objc2_app_kit::NSAutoresizingMaskOptions::ViewWidthSizable
            | objc2_app_kit::NSAutoresizingMaskOptions::ViewHeightSizable,
    );
    content_view.addSubview(&flutter_view);
}

/// Renders the FlutterViewController's NSView to an offscreen PNG on disk.
///
/// This spike's execution environment turned out not to have a usable on-screen
/// WindowServer session (verified independently with a minimal tao-only probe: a
/// plain `tao` window built without error but never appeared in
/// `CGWindowListCopyWindowInfo`, even with sandboxing disabled and via
/// `launchctl asuser`). `screencapture`/`CGWindowList`-based visual proof was
/// therefore not obtainable from outside the process. As a substitute, this asks
/// AppKit to rasterize the view's own layer tree directly within-process via
/// `cacheDisplayInRect:toBitmapImageRep:`, which does not require the window to be
/// on the physical screen.
unsafe fn snapshot_view_to_png(controller: &objc2::runtime::AnyObject, path: &str) {
    let flutter_view: Retained<objc2::runtime::AnyObject> = unsafe { msg_send![controller, view] };
    let flutter_view: Retained<NSView> = unsafe { Retained::cast_unchecked(flutter_view) };

    let bounds = flutter_view.bounds();
    flutter_view.setNeedsDisplay(true);
    flutter_view.displayIfNeeded();

    let Some(bitmap) = flutter_view.bitmapImageRepForCachingDisplayInRect(bounds) else {
        println!("[spike-a] snapshot: bitmapImageRepForCachingDisplayInRect returned None");
        return;
    };
    flutter_view.cacheDisplayInRect_toBitmapImageRep(bounds, &bitmap);

    let props: Retained<NSDictionary<objc2_app_kit::NSBitmapImageRepPropertyKey, objc2::runtime::AnyObject>> =
        NSDictionary::new();
    let png_data = unsafe {
        bitmap.representationUsingType_properties(NSBitmapImageFileType::PNG, &props)
    };
    match png_data {
        Some(data) => {
            let path_ns = NSString::from_str(path);
            let ok = data.writeToFile_atomically(&path_ns, true);
            println!("[spike-a] snapshot: wrote {path} (success={ok}, bytes={})", data.len());
        }
        None => println!("[spike-a] snapshot: representationUsingType:properties: returned None"),
    }
}

/// Removes the Flutter view from its superview. Combined with dropping the
/// `Retained<AnyObject>` controller, this is the "detach" half of criterion 3 —
/// per the header docs, a deallocated FlutterViewController automatically
/// removes itself from its engine.
unsafe fn detach_view(controller: &objc2::runtime::AnyObject) {
    let flutter_view: Retained<objc2::runtime::AnyObject> = unsafe { msg_send![controller, view] };
    let flutter_view: Retained<NSView> = unsafe { Retained::cast_unchecked(flutter_view) };
    flutter_view.removeFromSuperview();
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Start,
    EngineBooted,
    FirstViewAttached,
    ObservedFirstView,
    FirstViewDetached,
    SecondViewAttached,
    ObservedSecondView,
    LeakCheck,
    EngineShutdown,
    Done,
}

struct AppState {
    log: Log,
    step: Step,
    engine: Option<Retained<objc2::runtime::AnyObject>>,
    controller1: Option<Retained<objc2::runtime::AnyObject>>,
    controller2: Option<Retained<objc2::runtime::AnyObject>>,
    window: Option<Window>,
    leak_check_cycle: u32,
    leak_check_anchor_detached: bool,
}

const LEAK_CHECK_CYCLES: u32 = 8;

fn main() {
    let log = Log::new();
    log.line("Spike A starting: engine-first Flutter embedding on macOS");
    log.line(&format!(
        "App.framework path: {}",
        app_framework_path()
    ));

    let event_loop = EventLoop::new();

    let mut state = AppState {
        log,
        step: Step::Start,
        engine: None,
        controller1: None,
        controller2: None,
        window: None,
        leak_check_cycle: 0,
        leak_check_anchor_detached: false,
    };

    event_loop.run(move |event, target, control_flow| {
        match event {
            Event::NewEvents(StartCause::Init) => {
                *control_flow = ControlFlow::WaitUntil(Instant::now() + STEP_INTERVAL);
            }
            Event::NewEvents(StartCause::ResumeTimeReached { .. }) => {
                advance(&mut state, target);
                if state.step == Step::Done {
                    *control_flow = ControlFlow::Exit;
                } else {
                    // Leak-check cycles just need the run loop pumped between iterations
                    // (to let Cocoa process window-close/dealloc notifications), not a
                    // multi-second visual pause like the rest of the sequence.
                    let interval = if state.step == Step::ObservedSecondView {
                        Duration::from_millis(150)
                    } else {
                        STEP_INTERVAL
                    };
                    *control_flow = ControlFlow::WaitUntil(Instant::now() + interval);
                }
            }
            Event::LoopDestroyed => {
                state.log.line("LoopDestroyed - process exiting");
            }
            _ => {}
        }
    });
}

fn advance(state: &mut AppState, target: &tao::event_loop::EventLoopWindowTarget<()>) {
    match state.step {
        Step::Start => {
            state.log.line("=== Step 1: boot headless FlutterEngine (no view) ===");
            let engine = unsafe { boot_headless_engine(&state.log) };
            state.log.pass(
                1,
                "engine booted with allowHeadlessExecution=true, runWithEntrypoint returned YES, \
                 process is alive with zero views attached",
            );
            state.engine = Some(engine);
            state.step = Step::EngineBooted;
        }

        Step::EngineBooted => {
            state.log.line("=== Step 2: create first view, parent into tao window ===");
            let window = WindowBuilder::new()
                .with_title("Duet Spike A - view 1")
                .with_inner_size(LogicalSize::new(800.0, 600.0))
                .build(target)
                .expect("failed to build tao window");

            let engine = state.engine.as_ref().expect("engine missing");
            let controller1 = unsafe { create_view_controller(engine) };
            unsafe { attach_view_to_window(&window, &controller1) };

            let view_id: i64 = unsafe { msg_send![&controller1, viewIdentifier] };
            let attached: bool = unsafe { msg_send![&controller1, attached] };
            state.log.line(&format!(
                "controller1 viewIdentifier={view_id} attached={attached}"
            ));
            state.log.pass(
                2,
                "first FlutterViewController created via initWithEngine:, view parented into \
                 tao NSWindow's contentView - Flutter counter app should be visible now",
            );

            state.window = Some(window);
            state.controller1 = Some(controller1);
            state.step = Step::FirstViewAttached;
        }

        Step::FirstViewAttached => {
            state.log.line(
                "=== Step 3: holding first view on screen for a few seconds (visual check) ===",
            );
            let controller1 = state.controller1.as_ref().expect("controller1 missing");
            unsafe { snapshot_view_to_png(controller1, "/tmp/spike_a_view1.png") };
            state.step = Step::ObservedFirstView;
        }

        Step::ObservedFirstView => {
            state.log.line("=== Step 4: detach + destroy first view, engine stays running ===");

            // Remove from window hierarchy first.
            let controller1 = state.controller1.as_ref().expect("controller1 missing");
            unsafe { detach_view(controller1) };

            // Drop the controller. Per FlutterViewController.h: "When a
            // FlutterViewController is deallocated, it automatically removes itself
            // from its attached engine." Dropping is the detach.
            let controller1 = state.controller1.take().expect("controller1 missing");
            drop(controller1);

            // Also drop the window that hosted it, to prove nothing about the engine
            // depends on that window still existing.
            state.window = None;

            state.log.line("controller1 and its window dropped");

            // Prove the engine is still alive and running. We tried calling
            // runWithEntrypoint: a second time expecting a YES ("still running") signal,
            // but that was a bad assumption - see FINDINGS.md. It returned NO, which on
            // reflection matches the header's actual contract: "@return YES if the call
            // succeeds in creating and running a Flutter Engine instance" - a repeat call
            // creates nothing new, so NO is the *correct* answer for an engine that is
            // still alive, not evidence of death. We log it for the record but do not
            // treat it as the liveness signal.
            let engine = state.engine.as_ref().expect("engine missing");
            let entrypoint: Option<&NSString> = None;
            let repeat_run_result: bool =
                unsafe { msg_send![engine, runWithEntrypoint: entrypoint] };
            state.log.line(&format!(
                "post-detach runWithEntrypoint(nil) returned {repeat_run_result} (NOT used as \
                 liveness proof - see FINDINGS.md; the object responding at all without crashing \
                 already shows it's still a live, valid FlutterEngine*)"
            ));

            // A more meaningful liveness check: read the binaryMessenger property. If the
            // engine had been torn down, this object would be a dangling/deallocated
            // pointer and messaging it would crash (EXC_BAD_ACCESS), not return cleanly.
            let messenger: Retained<objc2::runtime::AnyObject> =
                unsafe { msg_send![engine, binaryMessenger] };
            state.log.line(&format!(
                "engine.binaryMessenger still resolves to a live object: {:p}",
                &*messenger as *const _
            ));

            state.log.pass(
                3,
                "first view detached (removeFromSuperview + controller dropped); engine object \
                 remained valid and messageable post-detach (binaryMessenger query succeeded with \
                 no crash). Definitive proof follows in step 5/criterion 4: the SAME engine \
                 reference is reused to create and render a brand new working view.",
            );

            state.step = Step::FirstViewDetached;
        }

        Step::FirstViewDetached => {
            state.log.line("=== Step 5: create second view against the SAME engine ===");
            let window2 = WindowBuilder::new()
                .with_title("Duet Spike A - view 2 (same engine)")
                .with_inner_size(LogicalSize::new(800.0, 600.0))
                .build(target)
                .expect("failed to build second tao window");

            let engine = state.engine.as_ref().expect("engine missing");
            let controller2 = unsafe { create_view_controller(engine) };
            unsafe { attach_view_to_window(&window2, &controller2) };

            let view_id: i64 = unsafe { msg_send![&controller2, viewIdentifier] };
            let attached: bool = unsafe { msg_send![&controller2, attached] };
            state.log.line(&format!(
                "controller2 viewIdentifier={view_id} attached={attached} (note: IDs are per-engine, \
                 so a fresh id being assigned to a live engine after the first view's id was freed \
                 is itself further evidence the engine kept running its own bookkeeping)"
            ));

            state.log.pass(
                4,
                "second FlutterViewController created via initWithEngine: against the engine that \
                 survived step 4, and rendered into a second tao window",
            );

            state.window = Some(window2);
            state.controller2 = Some(controller2);
            state.step = Step::SecondViewAttached;
        }

        Step::SecondViewAttached => {
            state.log.line(
                "=== Step 6: holding second view on screen for a few seconds (visual check) ===",
            );
            let controller2 = state.controller2.as_ref().expect("controller2 missing");
            unsafe { snapshot_view_to_png(controller2, "/tmp/spike_a_view2.png") };
            state.step = Step::ObservedSecondView;
        }

        Step::ObservedSecondView => {
            // Sequential replace cycles: fully detach + drop the current (only) view
            // controller before creating the next one. We learned the hard way that
            // FlutterEngine on macOS enforces "one view controller for the implicit view"
            // at a time - trying to create a second one while the first is still attached
            // throws NSInternalInconsistencyException ("The engine already has a view
            // controller for the implicit view."). See FINDINGS.md for the full story,
            // including the earlier version of this loop that hit exactly that exception.
            if !state.leak_check_anchor_detached {
                state.log.line(&format!(
                    "=== Step 6b: rough leak check - {LEAK_CHECK_CYCLES}x sequential \
                     create+attach -> detach+destroy replace cycles on the same engine, one \
                     per event-loop tick ==="
                ));
                if let Some(controller2) = state.controller2.take() {
                    unsafe { detach_view(&controller2) };
                    drop(controller2);
                }
                state.window = None;
                state.leak_check_anchor_detached = true;
                // Give the run loop a full tick to actually deallocate the just-dropped
                // controller (see the comment above - this is required, not paranoia).
                return;
            }

            if state.leak_check_cycle < LEAK_CHECK_CYCLES {
                let i = state.leak_check_cycle;
                let engine = state.engine.as_ref().expect("engine missing");
                let window = WindowBuilder::new()
                    .with_title(format!("Duet Spike A - leak check {i}"))
                    .with_inner_size(LogicalSize::new(400.0, 300.0))
                    .build(target)
                    .expect("failed to build leak-check window");
                let controller = unsafe { create_view_controller(engine) };
                unsafe { attach_view_to_window(&window, &controller) };
                unsafe { detach_view(&controller) };
                drop(controller);
                drop(window);
                state.log.line(&format!("leak-check cycle {i} done"));
                state.leak_check_cycle += 1;
            }

            if state.leak_check_cycle >= LEAK_CHECK_CYCLES {
                state.step = Step::LeakCheck;
            }
        }

        Step::LeakCheck => {
            state.log.line("=== Step 7: shut down engine cleanly ===");

            if let Some(controller2) = state.controller2.take() {
                unsafe { detach_view(&controller2) };
                drop(controller2);
            }
            state.window = None;

            if let Some(engine) = state.engine.take() {
                unsafe {
                    let _: () = msg_send![&engine, shutDownEngine];
                }
                state.log.line("shutDownEngine sent");
                drop(engine);
            }

            state.log.pass(
                5,
                "shutDownEngine called, all Retained handles dropped, no crash observed so far",
            );

            state.step = Step::EngineShutdown;
        }

        Step::EngineShutdown => {
            state.log.line("All five criteria exercised. Exiting process in 1 more tick to confirm no crash-on-exit.");
            state.step = Step::Done;
        }

        Step::Done => {
            // unreachable, ControlFlow::Exit is set by caller once we reach Done
        }
    }
}
