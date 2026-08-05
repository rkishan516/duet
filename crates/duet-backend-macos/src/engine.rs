//! Flutter engine and view controller lifetime.
//!
//! Ports the API sequence Spike A (`spikes/spike-a-macos/src/main.rs`) proved
//! on this machine:
//!
//! ```text
//! FlutterDartProject    initWithPrecompiledDartBundle:  <NSBundle of App.framework>
//! FlutterEngine         initWithName:project:allowHeadlessExecution:YES
//! FlutterEngine         runWithEntrypoint:nil                        -> BOOL
//! FlutterViewController initWithEngine:nibName:bundle:                (per view)
//!   .view -> NSView, addSubview into the tao NSWindow's contentView
//! detach = removeFromSuperview + drop the controller (engine holds it only weakly)
//! FlutterEngine         shutDownEngine
//! ```
//!
//! Every call here must run on the main thread (AppKit and the Flutter
//! platform thread both require it — see spec §6.2). Callers are
//! responsible for that; this module cannot enforce it itself, since neither
//! `tao` nor `objc2` expose a portable "is this the main thread" check that
//! would be worth trusting more than the caller's own discipline.

use std::panic::{self, AssertUnwindSafe};

use duet_host::BackendError;
use objc2::msg_send;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject};
use objc2_app_kit::{NSAutoresizingMaskOptions, NSView, NSWindow};
use objc2_foundation::{NSBundle, NSString};
use tao::platform::macos::WindowExtMacOS;

/// Owns one Flutter engine and, at most, one attached view.
///
/// The engine is created headless (`allowHeadlessExecution: true`) and a
/// view controller is attached separately, which is what makes teardown
/// independent of window lifetime — an engine can outlive a window close and
/// a window can be recreated without rebooting the engine (see
/// [`FlutterEngine::detach`]).
///
/// **One view per engine.** Spike A confirmed that a second concurrent
/// `initWithEngine:` throws `NSInternalInconsistencyException` ("The engine
/// already has a view controller for the implicit view") — the multi-view
/// API accepts only one view controller for the engine's implicit view at a
/// time, sequentially. Duet's `MacBackend` therefore uses one engine per
/// Flutter window rather than the multi-view path the headers appear to
/// offer.
pub(crate) struct FlutterEngine {
    /// The `FlutterEngine*` (an `AnyObject` because objc2 has no typed
    /// binding for it — it comes from `FlutterMacOS.framework`, which this
    /// crate links via `build.rs` but does not have `objc2` bindgen output
    /// for).
    engine: Retained<AnyObject>,
    /// The currently attached `FlutterViewController*`, if any.
    controller: Option<Retained<AnyObject>>,
    /// Whether [`FlutterEngine::shutdown`] has already run, so `Drop` does
    /// not send `shutDownEngine` a second time.
    shut_down: bool,
}

impl FlutterEngine {
    /// Boots an engine with no view, from the assets in `app_framework`.
    ///
    /// `app_framework` is a filesystem path to Flutter's `App.framework`
    /// bundle (produced by `flutter build macos`), whose `Info.plist`
    /// carries `CFBundleIdentifier = io.flutter.flutter.app`. macOS's
    /// `FlutterDartProject` has no assets-path API, so the bundle is how
    /// assets are located.
    ///
    /// # Errors
    ///
    /// [`BackendError::Unavailable`] if the bundle could not be loaded, the
    /// Flutter classes are not linked, `runWithEntrypoint:` returned `NO`, or
    /// an Objective-C exception was thrown and caught by `objc2`'s
    /// `catch-all` feature (which turns it into a Rust panic that this
    /// function catches and converts, rather than letting it abort the
    /// process or propagate as a panic across this API boundary).
    pub(crate) fn boot(app_framework: &str) -> Result<Self, BackendError> {
        // SAFETY: `boot_uncaught` requires the caller to be on the main
        // thread, which this crate documents as a precondition of every
        // `FlutterEngine` method (see the module docs) rather than something
        // it can check.
        let engine = catch_to_backend_error(|| unsafe { boot_uncaught(app_framework) })??;
        Ok(FlutterEngine {
            engine,
            controller: None,
            shut_down: false,
        })
    }

    /// Creates a view controller and adds its `NSView` to `window`'s content
    /// view.
    ///
    /// # Errors
    ///
    /// [`BackendError::Unavailable`] if a view is already attached (callers
    /// must [`FlutterEngine::detach`] first — see the one-view-per-engine
    /// constraint on this type), if the view controller or its view could
    /// not be created, if `window` has no backing `NSWindow`, or if an
    /// Objective-C exception was caught.
    pub(crate) fn attach(&mut self, window: &tao::window::Window) -> Result<(), BackendError> {
        if self.controller.is_some() {
            return Err(BackendError::Unavailable(
                "a view is already attached to this engine — detach it first".to_string(),
            ));
        }
        let engine = &self.engine;
        // SAFETY: `attach_uncaught` requires the caller to be on the main
        // thread and `engine` to be a live, valid `FlutterEngine*` with no
        // view controller currently attached. `engine` came from `self`,
        // which is only ever constructed by `boot` from a successfully
        // initialized engine, and the `self.controller.is_some()` check just
        // above enforces the "no view controller attached" half.
        let controller = catch_to_backend_error(|| unsafe { attach_uncaught(engine, window) })??;
        self.controller = Some(controller);
        Ok(())
    }

    /// Removes the view from its superview and drops the controller.
    ///
    /// Spike A measured that this reclaims essentially nothing — 223 MB
    /// before and after. Only [`FlutterEngine::shutdown`] frees memory.
    ///
    /// Infallible by design (matching the plan's shape for this type): if
    /// `removeFromSuperview` throws, the exception is caught (via `objc2`'s
    /// `catch-all`) and absorbed rather than propagated, because the
    /// controller is dropped immediately afterwards regardless — per
    /// `FlutterViewController.h`, a deallocated controller automatically
    /// removes itself from its engine, so there is no recovery action a
    /// caller could take differently based on whether the explicit
    /// `removeFromSuperview` succeeded.
    pub(crate) fn detach(&mut self) {
        let Some(controller) = self.controller.take() else {
            return;
        };
        // SAFETY: `detach_uncaught` requires the caller to be on the main
        // thread and `controller` to be a valid, live
        // `FlutterViewController*` with an attached `.view`. `controller`
        // was just taken from `self.controller`, which `attach` only ever
        // populates with a controller it successfully created and attached.
        let _ = panic::catch_unwind(AssertUnwindSafe(|| unsafe {
            detach_uncaught(&controller);
        }));
        // `controller` drops here either way.
    }

    /// Shuts the engine down. **This is what reclaims memory** — Spike A
    /// measured 223 MB before and 104 MB after.
    ///
    /// Detaches any still-attached view first, so `shutdown` is safe to call
    /// without a preceding `detach`. Idempotent: a second call is a no-op.
    ///
    /// Infallible by design, for the same reason as [`FlutterEngine::detach`]:
    /// there is no distinct recovery path for a caught `shutDownEngine`
    /// exception, and the engine handle is dropped by the caller immediately
    /// after regardless.
    pub(crate) fn shutdown(&mut self) {
        self.detach();
        if self.shut_down {
            return;
        }
        self.shut_down = true;
        let engine = &self.engine;
        // SAFETY: `engine` is `self`'s own live `FlutterEngine*`, and this
        // method requires the caller to be on the main thread, per this
        // type's module-level contract. `shutDownEngine` takes no
        // arguments and its return type is `void`, matching the `()`
        // annotation below.
        let _ = panic::catch_unwind(AssertUnwindSafe(|| unsafe {
            let _: () = msg_send![engine, shutDownEngine];
        }));
    }
}

impl Drop for FlutterEngine {
    /// Guarantees `shutDownEngine` is sent even if a caller forgets to call
    /// [`FlutterEngine::shutdown`] explicitly (for example on an early
    /// `?`-propagated error path elsewhere in the backend) — an engine leaked
    /// past its `Retained` handle would otherwise never reclaim its 100+ MB.
    fn drop(&mut self) {
        if !self.shut_down {
            self.shutdown();
        }
    }
}

/// Runs `f`, converting a caught Objective-C exception (a Rust panic, once
/// `objc2`'s `catch-all` feature is enabled — see the crate root docs) into a
/// [`BackendError`] instead of letting it unwind further.
///
/// `AssertUnwindSafe` is used because the closures passed here only read
/// shared references and construct fresh `Retained` handles; none of them
/// leave a shared `&mut` in an inconsistent state if the unsafe call inside
/// panics; the panic happens inside `objc2`'s `@catch` before any partially
/// constructed value here is observed by a caller.
fn catch_to_backend_error<F, R>(f: F) -> Result<R, BackendError>
where
    F: FnOnce() -> R,
{
    panic::catch_unwind(AssertUnwindSafe(f)).map_err(|payload| {
        let reason = panic_message(&payload);
        BackendError::Unavailable(format!(
            "Objective-C exception caught during Flutter engine call: {reason}"
        ))
    })
}

/// Extracts a human-readable message from a caught panic payload.
///
/// `objc2`'s `catch-all` panics with a message built from the
/// `NSException`'s name and reason where available (see `objc2::exception`),
/// which downcasts as `&str` or `String` depending on how it was formatted.
/// Neither is guaranteed, so this falls back to a generic message rather
/// than unwrapping.
fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic (no string payload)".to_string()
    }
}

/// Builds a `FlutterDartProject` from `app_framework`'s `NSBundle`.
///
/// # Safety
///
/// Caller must be on the main thread, and `FlutterMacOS.framework` must be
/// linked (this crate's `build.rs` guarantees the link; it cannot guarantee
/// the thread).
unsafe fn build_dart_project(app_framework: &str) -> Result<Retained<AnyObject>, BackendError> {
    let path_ns = NSString::from_str(app_framework);
    let bundle = NSBundle::bundleWithPath(&path_ns).ok_or_else(|| {
        BackendError::Unavailable(format!(
            "NSBundle::bundleWithPath failed for {app_framework}"
        ))
    })?;

    let project_cls = AnyClass::get(c"FlutterDartProject").ok_or_else(|| {
        BackendError::Unavailable(
            "FlutterDartProject class not found - is FlutterMacOS.framework linked?".to_string(),
        )
    })?;
    // SAFETY: `project_cls` was just resolved from the live Objective-C
    // runtime, and `alloc` is safe to send to any class.
    let alloc: Allocated<AnyObject> = unsafe { msg_send![project_cls, alloc] };
    // SAFETY: `alloc` produced a freshly allocated, uninitialized
    // `FlutterDartProject*`; `initWithPrecompiledDartBundle:` is its
    // designated initializer and `bundle` is a live `NSBundle`.
    let project: Retained<AnyObject> =
        unsafe { msg_send![alloc, initWithPrecompiledDartBundle: &*bundle] };
    Ok(project)
}

/// Boots a headless (viewless) `FlutterEngine`. `allowHeadlessExecution:
/// true` is what lets the engine exist with zero views — see the crate
/// root's constraint list.
///
/// # Safety
///
/// Caller must be on the main thread.
unsafe fn boot_uncaught(app_framework: &str) -> Result<Retained<AnyObject>, BackendError> {
    // SAFETY: delegated to `build_dart_project`'s own contract; this
    // function's caller has already established we are on the main thread.
    let project = unsafe { build_dart_project(app_framework)? };

    let engine_cls = AnyClass::get(c"FlutterEngine").ok_or_else(|| {
        BackendError::Unavailable(
            "FlutterEngine class not found - is FlutterMacOS.framework linked?".to_string(),
        )
    })?;
    // SAFETY: `engine_cls` was just resolved from the live runtime.
    let alloc: Allocated<AnyObject> = unsafe { msg_send![engine_cls, alloc] };
    let name = NSString::from_str("duet-backend-macos");
    // SAFETY: `alloc` is a freshly allocated `FlutterEngine*`;
    // `initWithName:project:allowHeadlessExecution:` is a designated
    // initializer, `name` is a live `NSString`, and `project` is the live
    // `FlutterDartProject` built above. Passing `allowHeadlessExecution:
    // true` — constraint 1 from the crate root docs — is what lets this
    // engine exist with zero attached views.
    let engine: Retained<AnyObject> = unsafe {
        msg_send![
            alloc,
            initWithName: &*name,
            project: &*project,
            allowHeadlessExecution: true,
        ]
    };

    let entrypoint: Option<&NSString> = None;
    // SAFETY: `engine` is the live object just initialized above;
    // `runWithEntrypoint:` accepts `nil` to mean "use main()".
    let ran: bool = unsafe { msg_send![&engine, runWithEntrypoint: entrypoint] };
    if !ran {
        return Err(BackendError::Unavailable(
            "FlutterEngine runWithEntrypoint(nil) returned NO".to_string(),
        ));
    }

    Ok(engine)
}

/// Creates a `FlutterViewController` via the multi-view
/// `initWithEngine:nibName:bundle:` path (constraint 2: never the legacy
/// `viewController` property) and parents its `NSView` into `window`'s
/// content view, filling it and tracking resizes.
///
/// # Safety
///
/// Caller must be on the main thread; `engine` must be a valid, still-alive
/// `FlutterEngine*` with no view controller currently attached (constraint 3
/// — the caller, [`FlutterEngine::attach`], already enforces this at the
/// Rust level via the `self.controller.is_some()` check); `window` must
/// currently be alive.
unsafe fn attach_uncaught(
    engine: &AnyObject,
    window: &tao::window::Window,
) -> Result<Retained<AnyObject>, BackendError> {
    let cls = AnyClass::get(c"FlutterViewController").ok_or_else(|| {
        BackendError::Unavailable("FlutterViewController class not found".to_string())
    })?;
    // SAFETY: `cls` was just resolved from the live runtime.
    let alloc: Allocated<AnyObject> = unsafe { msg_send![cls, alloc] };
    let nib_name: Option<&NSString> = None;
    let bundle: Option<&NSBundle> = None;
    // SAFETY: `alloc` is a freshly allocated `FlutterViewController*`;
    // `initWithEngine:nibName:bundle:` is the designated multi-view
    // initializer and `engine` is a live, valid `FlutterEngine*` per this
    // function's contract.
    let controller: Retained<AnyObject> = unsafe {
        msg_send![
            alloc,
            initWithEngine: engine,
            nibName: nib_name,
            bundle: bundle,
        ]
    };

    // SAFETY: `controller` was just successfully initialized above, so
    // `.view` is safe to send to it.
    let flutter_view: Retained<AnyObject> = unsafe { msg_send![&controller, view] };
    // SAFETY: `FlutterViewController.view` is documented to return an
    // `NSView*` at runtime; `objc2-app-kit` has no binding for
    // `FlutterViewController` itself (it is not part of AppKit), so the
    // handle arrives untyped and is cast to the typed binding here to reach
    // `NSView`'s typed methods below.
    let flutter_view: Retained<NSView> = unsafe { Retained::cast_unchecked(flutter_view) };

    let ns_window_ptr = window.ns_window() as *mut NSWindow;
    if ns_window_ptr.is_null() {
        return Err(BackendError::Unavailable(
            "tao window has no backing NSWindow".to_string(),
        ));
    }
    // SAFETY: `ns_window_ptr` is non-null and, per `tao::WindowExtMacOS`'s
    // contract, points at the live `NSWindow*` backing `window` for as long
    // as `window` itself is alive, which this function's caller guarantees.
    let ns_window: &NSWindow = unsafe { &*ns_window_ptr };
    let content_view = ns_window
        .contentView()
        .ok_or_else(|| BackendError::Unavailable("NSWindow has no contentView".to_string()))?;

    let bounds = content_view.bounds();
    flutter_view.setFrame(bounds);
    // NSViewWidthSizable | NSViewHeightSizable so the Flutter view tracks
    // window resizes.
    flutter_view.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable,
    );
    content_view.addSubview(&flutter_view);

    Ok(controller)
}

/// Removes the Flutter view from its superview. Combined with the caller
/// dropping the `Retained` controller, this is the "detach" half of
/// constraint 3 — per `FlutterViewController.h`, a deallocated
/// `FlutterViewController` automatically removes itself from its engine.
///
/// # Safety
///
/// Caller must be on the main thread; `controller` must be a valid, live
/// `FlutterViewController*` with an attached `.view`.
unsafe fn detach_uncaught(controller: &AnyObject) {
    // SAFETY: `controller` is a live `FlutterViewController*` per this
    // function's contract.
    let flutter_view: Retained<AnyObject> = unsafe { msg_send![controller, view] };
    // SAFETY: same reasoning as in `attach_uncaught` — `.view` is an
    // `NSView*` at runtime.
    let flutter_view: Retained<NSView> = unsafe { Retained::cast_unchecked(flutter_view) };
    flutter_view.removeFromSuperview();
}
