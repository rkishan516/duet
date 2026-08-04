//! Flutter engine embedding: boot, view attach, snapshot, and method
//! channels. Copied and adapted from spike-a-macos / spike-b-macos, which
//! already established this exact call sequence works (see their
//! FINDINGS.md for the "why" of each call). New here: a native handler
//! registered via `setMethodCallHandler:` to RECEIVE calls initiated from
//! the Dart side (spike-a/b only ever called Dart, never the other way).

use block2::RcBlock;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject};
use objc2::msg_send;
use objc2_app_kit::{NSBitmapImageFileType, NSView, NSWindow};
use objc2_foundation::{NSBundle, NSDictionary, NSNumber, NSRect, NSString};
use tao::platform::macos::WindowExtMacOS;
use tao::window::Window;

pub unsafe fn build_dart_project(app_framework_path: &str) -> Retained<AnyObject> {
    let path_ns = NSString::from_str(app_framework_path);
    let bundle: Retained<NSBundle> = NSBundle::bundleWithPath(&path_ns)
        .unwrap_or_else(|| panic!("NSBundle::bundleWithPath failed for {app_framework_path}"));

    let project_cls = AnyClass::get(c"FlutterDartProject")
        .expect("FlutterDartProject class not found - is FlutterMacOS.framework linked?");
    let alloc: Allocated<AnyObject> = unsafe { msg_send![project_cls, alloc] };
    let project: Retained<AnyObject> =
        unsafe { msg_send![alloc, initWithPrecompiledDartBundle: &*bundle] };
    project
}

pub unsafe fn boot_headless_engine(app_framework_path: &str, engine_name: &str) -> Retained<AnyObject> {
    let project = unsafe { build_dart_project(app_framework_path) };

    let engine_cls = AnyClass::get(c"FlutterEngine")
        .expect("FlutterEngine class not found - is FlutterMacOS.framework linked?");
    let alloc: Allocated<AnyObject> = unsafe { msg_send![engine_cls, alloc] };
    let name = NSString::from_str(engine_name);
    let engine: Retained<AnyObject> = unsafe {
        msg_send![
            alloc,
            initWithName: &*name,
            project: &*project,
            allowHeadlessExecution: true,
        ]
    };

    let entrypoint: Option<&NSString> = None;
    let ran: bool = unsafe { msg_send![&engine, runWithEntrypoint: entrypoint] };
    assert!(ran, "FlutterEngine failed to run entrypoint");

    engine
}

pub unsafe fn create_view_controller(engine: &AnyObject) -> Retained<AnyObject> {
    let cls = AnyClass::get(c"FlutterViewController").expect("FlutterViewController class not found");
    let alloc: Allocated<AnyObject> = unsafe { msg_send![cls, alloc] };
    let nib_name: Option<&NSString> = None;
    let bundle: Option<&NSBundle> = None;
    let controller: Retained<AnyObject> = unsafe {
        msg_send![
            alloc,
            initWithEngine: engine,
            nibName: nib_name,
            bundle: bundle,
        ]
    };
    controller
}

pub unsafe fn attach_view_to_window(window: &Window, controller: &AnyObject) -> Retained<NSView> {
    let flutter_view: Retained<AnyObject> = unsafe { msg_send![controller, view] };
    let flutter_view: Retained<NSView> = unsafe { Retained::cast_unchecked(flutter_view) };

    let ns_window_ptr = window.ns_window() as *mut NSWindow;
    let ns_window: &NSWindow = unsafe { &*ns_window_ptr };
    let content_view: Retained<NSView> = ns_window.contentView().expect("NSWindow has no contentView");

    let bounds: NSRect = content_view.bounds();
    flutter_view.setFrame(bounds);
    flutter_view.setAutoresizingMask(
        objc2_app_kit::NSAutoresizingMaskOptions::ViewWidthSizable
            | objc2_app_kit::NSAutoresizingMaskOptions::ViewHeightSizable,
    );
    content_view.addSubview(&flutter_view);
    flutter_view
}

/// Rasterizes an NSView to a PNG on disk via `cacheDisplayInRect:toBitmapImageRep:`.
/// Reused from spike-a/b: this environment has no reachable on-screen WindowServer,
/// so this in-process rasterization is the only way to get genuine pixel evidence.
pub unsafe fn snapshot_nsview_to_png(view: &NSView, path: &str) -> bool {
    let bounds = view.bounds();
    view.setNeedsDisplay(true);
    view.displayIfNeeded();

    let Some(bitmap) = view.bitmapImageRepForCachingDisplayInRect(bounds) else {
        eprintln!("[spike-c] snapshot: bitmapImageRepForCachingDisplayInRect returned None for {path}");
        return false;
    };
    view.cacheDisplayInRect_toBitmapImageRep(bounds, &bitmap);

    let props: Retained<NSDictionary<objc2_app_kit::NSBitmapImageRepPropertyKey, AnyObject>> =
        NSDictionary::new();
    let png_data = unsafe { bitmap.representationUsingType_properties(NSBitmapImageFileType::PNG, &props) };
    match png_data {
        Some(data) => {
            let path_ns = NSString::from_str(path);
            let ok = data.writeToFile_atomically(&path_ns, true);
            eprintln!("[spike-c] snapshot: wrote {path} (success={ok}, bytes={})", data.len());
            ok
        }
        None => {
            eprintln!("[spike-c] snapshot: representationUsingType:properties: returned None for {path}");
            false
        }
    }
}

pub unsafe fn make_method_channel(engine: &AnyObject, name: &str) -> Retained<AnyObject> {
    let messenger: Retained<AnyObject> = unsafe { msg_send![engine, binaryMessenger] };
    let cls = AnyClass::get(c"FlutterMethodChannel").expect("FlutterMethodChannel class not found");
    let name_ns = NSString::from_str(name);
    let channel: Retained<AnyObject> = unsafe {
        msg_send![cls, methodChannelWithName: &*name_ns, binaryMessenger: &*messenger]
    };
    channel
}

/// Fire-and-forget `invokeMethod:arguments:result:` call with no argument,
/// used to drive the fixture app's `_tapCount` up via its 'increment'
/// handler on `duet/spike_b` - a deterministic stand-in for real tap input
/// (spike-b found synthetic NSEvents unreliable in this windowless
/// environment).
pub unsafe fn invoke_no_args(channel: &AnyObject, method: &str) {
    let method_ns = NSString::from_str(method);
    let no_args: Option<&AnyObject> = None;
    let block = RcBlock::new(move |_result: *mut AnyObject| {});
    unsafe {
        let _: () = msg_send![channel, invokeMethod: &*method_ns, arguments: no_args, result: &*block];
    }
}

/// Registers a native handler for calls INITIATED BY DART (the fixture
/// app's `_spikeCChannel.invokeMethod('frameMarker', {...})`, fired from a
/// re-registering `addPostFrameCallback` after every frame). This is the
/// mechanism the latency probe depends on: it's the closest signal to
/// "the changed UI has actually rendered a frame" that a platform channel
/// can observe.
///
/// We deliberately do NOT invoke the `result` block passed to us (it's the
/// `FlutterResult` completion callback for Dart's `invokeMethod` future).
/// Calling an incoming Objective-C block from Rust needs extra ceremony this
/// throwaway spike doesn't bother with; the Dart side never awaits this
/// call, so leaving it unresolved is harmless.
pub unsafe fn register_frame_marker_handler(channel: &AnyObject, on_frame: impl Fn(String, i64) + 'static) {
    let block = RcBlock::new(move |call: *mut AnyObject, _result: *mut AnyObject| {
        if call.is_null() {
            return;
        }
        let method: Retained<NSString> = unsafe { msg_send![call, method] };
        if method.to_string() != "frameMarker" {
            return;
        }
        let args: *mut AnyObject = unsafe { msg_send![call, arguments] };
        if args.is_null() {
            return;
        }
        let marker_key = NSString::from_str("marker");
        let tap_key = NSString::from_str("tapCount");
        let marker_val: *mut AnyObject = unsafe { msg_send![args, objectForKey: &*marker_key] };
        let tap_val: *mut AnyObject = unsafe { msg_send![args, objectForKey: &*tap_key] };
        let marker = if marker_val.is_null() {
            String::new()
        } else {
            let ns_ptr = marker_val as *const NSString;
            unsafe { (&*ns_ptr).to_string() }
        };
        let tap_count: i64 = if tap_val.is_null() {
            -1
        } else {
            let num_ptr = tap_val as *const NSNumber;
            unsafe { (&*num_ptr).as_i64() }
        };
        on_frame(marker, tap_count);
    });
    unsafe {
        let _: () = msg_send![channel, setMethodCallHandler: &*block];
    }
}
