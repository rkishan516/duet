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
//! flutter/lifecycle     sendOnChannel:message:  "AppLifecycleState.inactive"
//! flutter/lifecycle     sendOnChannel:message:  "AppLifecycleState.hidden"    (before detach)
//! detach = removeFromSuperview + drop the controller (engine holds it only weakly)
//! flutter/lifecycle     sendOnChannel:message:  "AppLifecycleState.inactive"
//! flutter/lifecycle     sendOnChannel:message:  "AppLifecycleState.resumed"   (after attach)
//! FlutterEngine         shutDownEngine
//! ```
//!
//! The two `flutter/lifecycle` steps are not part of Spike A's sequence —
//! Spike A never left a view detached long enough to notice they were
//! missing (see `FINDINGS.md` F1 and its "Contradicts a Phase 0 finding"
//! section). Without them, Dart's scheduler keeps requesting frames after
//! `detach` removes the view, the embedder logs `Could not create the
//! embedder backing store` in an unbounded retry loop, and the host process
//! never gets control back. See [`FlutterEngine::detach`] and
//! [`FlutterEngine::attach`] for the fix and why the `inactive` step cannot
//! be skipped.
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
use objc2_foundation::{NSBundle, NSData, NSString};
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
/// This type is `pub` only so that [`crate::FlutterSurface::new`] can name it
/// in its signature (a `pub` function mentioning a `pub(crate)` type trips
/// `private_interfaces`). Every method on it stays `pub(crate)`: the engine's
/// lifecycle is driven exclusively through [`crate::MacBackend`]'s
/// [`duet_host::WindowBackend`] impl, and nothing outside this crate may
/// boot, attach, detach or shut one down directly.
pub struct FlutterEngine {
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
    /// view, then restarts Dart's frame scheduling.
    ///
    /// After the view is parented in, this sends `AppLifecycleState.inactive`
    /// then `AppLifecycleState.resumed` on `flutter/lifecycle`. The
    /// intermediate `inactive` step is not optional: the framework's
    /// transition table (`packages/flutter/lib/src/services/binding.dart`)
    /// only allows `hidden -> inactive -> resumed` (and, symmetrically for
    /// [`FlutterEngine::detach`], `resumed -> inactive -> hidden`) — sending
    /// `resumed` directly from `hidden` trips a framework assertion. See
    /// [`FlutterEngine::detach`]'s docs for the matching half of this and
    /// `FINDINGS.md` F1 for why this exists at all: without it, a
    /// previously-suspended engine's Dart scheduler stays disabled forever
    /// (harmless but wrong) rather than the storm this pair actually fixes,
    /// since `hidden` is what disabled it in the first place.
    ///
    /// # Errors
    ///
    /// [`BackendError::Unavailable`] if a view is already attached (callers
    /// must [`FlutterEngine::detach`] first — see the one-view-per-engine
    /// constraint on this type), if the view controller or its view could
    /// not be created, if `window` has no backing `NSWindow`, if an
    /// Objective-C exception was caught, or if either lifecycle message
    /// failed to send (the view is attached either way at that point — only
    /// the frame-scheduling restart is in question).
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
        self.set_lifecycle_state("AppLifecycleState.inactive")?;
        self.set_lifecycle_state("AppLifecycleState.resumed")?;
        Ok(())
    }

    /// Stops Dart's frame scheduling, then removes the view from its
    /// superview and drops the controller.
    ///
    /// Spike A measured that removing the view alone reclaims essentially
    /// nothing — 223 MB before and after. Only [`FlutterEngine::shutdown`]
    /// frees memory; the point of `detach` is to keep the engine alive so a
    /// later [`FlutterEngine::attach`] skips the ~180 ms engine boot.
    ///
    /// **This is the fix for `FINDINGS.md` F1.** Before touching the view,
    /// this sends `AppLifecycleState.inactive` then
    /// `AppLifecycleState.hidden` on `flutter/lifecycle`
    /// (`FlutterEngine`'s `binaryMessenger`, `StringCodec`-encoded raw UTF-8
    /// — see [`FlutterEngine::set_lifecycle_state`]). `hidden` is what
    /// `SchedulerBinding.handleAppLifecycleStateChanged`
    /// (`packages/flutter/lib/src/scheduler/binding.dart`) uses to disable
    /// frame scheduling (`_setFramesEnabledState(false)`). Without this,
    /// Dart keeps requesting a frame every vsync (any app with a running
    /// `Ticker`, like `spikes/spike_app`) with no view to composite into,
    /// and the embedder logs `Could not create the embedder backing store`
    /// in an apparently-unbounded retry loop that starves the host's own run
    /// loop — see `FINDINGS.md` F1 for the full measured reproduction.
    ///
    /// The `inactive` step is not optional, and must come *before* `hidden`:
    /// the framework's transition table
    /// (`packages/flutter/lib/src/services/binding.dart`) only allows
    /// `resumed -> inactive -> hidden`; sending `hidden` directly from
    /// `resumed` trips a framework assertion rather than silently working.
    /// A future edit that "simplifies" this to a single `hidden` send will
    /// reintroduce that assertion failure, not just lose the frame-stopping
    /// effect.
    ///
    /// # Errors
    ///
    /// Unlike Spike A's version of this method, `detach` is now fallible.
    /// Sending a lifecycle message is a real Objective-C call
    /// (`sendOnChannel:message:` through a live `binaryMessenger`) and can
    /// throw, unlike the old `removeFromSuperview`-only body. This method
    /// still *performs* the detach — `removeFromSuperview` and dropping the
    /// controller both run unconditionally below, same as before — but
    /// returns [`BackendError::Unavailable`] if either lifecycle send
    /// failed, because that failure means the F1 retry storm this method
    /// exists to prevent may still happen even though the view came off
    /// successfully. Contrast with `removeFromSuperview`'s own exception,
    /// caught and absorbed a few lines down: that failure has no distinct
    /// recovery action (the controller drops immediately after regardless),
    /// whereas a failed lifecycle send is the one thing in this method a
    /// caller (`MacBackend::detach_view`, which already returns `Result`)
    /// genuinely needs to know about.
    pub(crate) fn detach(&mut self) -> Result<(), BackendError> {
        let Some(controller) = self.controller.take() else {
            return Ok(());
        };
        let lifecycle_result = self
            .set_lifecycle_state("AppLifecycleState.inactive")
            .and_then(|()| self.set_lifecycle_state("AppLifecycleState.hidden"));
        // SAFETY: `detach_uncaught` requires the caller to be on the main
        // thread and `controller` to be a valid, live
        // `FlutterViewController*` with an attached `.view`. `controller`
        // was just taken from `self.controller`, which `attach` only ever
        // populates with a controller it successfully created and attached.
        let _ = panic::catch_unwind(AssertUnwindSafe(|| unsafe {
            detach_uncaught(&controller);
        }));
        // `controller` drops here either way.
        lifecycle_result
    }

    /// Shuts the engine down. **This is what reclaims memory** — Spike A
    /// measured 223 MB before and 104 MB after.
    ///
    /// Detaches any still-attached view first, so `shutdown` is safe to call
    /// without a preceding `detach`. Idempotent: a second call is a no-op.
    ///
    /// Infallible by design. This stays infallible even though
    /// [`FlutterEngine::detach`] (called first, below) is not: a failed
    /// lifecycle send there only means the F1 retry storm might still be
    /// running when `shutDownEngine` is sent, and `shutDownEngine` runs here
    /// regardless of that outcome — there is no different action `shutdown`
    /// could take with the error, since the engine handle is dropped by the
    /// caller immediately after either way. Same reasoning as the
    /// `shutDownEngine` exception itself, caught and absorbed below.
    pub(crate) fn shutdown(&mut self) {
        let _ = self.detach();
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

    /// Sends `state` as a raw UTF-8 message on the `flutter/lifecycle`
    /// channel, through this engine's `binaryMessenger`.
    ///
    /// `flutter/lifecycle` is a `BasicMessageChannel<String?>` using
    /// `StringCodec` (`packages/flutter/lib/src/services/system_channels.dart`),
    /// and `StringCodec` puts the payload on the wire as-is — raw UTF-8
    /// bytes, no method-call envelope, no length prefix
    /// (`packages/flutter/lib/src/services/message_codecs.dart`). That means
    /// there is no need to build a `FlutterBasicMessageChannel` on the
    /// Objective-C side at all: this sends `state`'s raw bytes directly
    /// through `sendOnChannel:message:`, the same `FlutterBinaryMessenger`
    /// primitive Spike B's method channel is built on top of
    /// (`spikes/spike-b-macos/src/main.rs`'s `make_method_channel`, which
    /// also starts from `[engine, binaryMessenger]`).
    ///
    /// Expected callers pass one of `"AppLifecycleState.inactive"`,
    /// `"AppLifecycleState.hidden"`, or `"AppLifecycleState.resumed"` — see
    /// [`FlutterEngine::detach`] and [`FlutterEngine::attach`] for why those
    /// three specifically, and why the order between them matters.
    ///
    /// # Errors
    ///
    /// [`BackendError::Unavailable`] if an Objective-C exception was caught
    /// (for example, a dead `binaryMessenger`).
    pub(crate) fn set_lifecycle_state(&self, state: &str) -> Result<(), BackendError> {
        let engine = &self.engine;
        // SAFETY: `send_lifecycle_state_uncaught` requires the caller to be
        // on the main thread and `engine` to be a live, valid
        // `FlutterEngine*`. `engine` came from `self`, which is only ever
        // constructed by `boot` from a successfully initialized engine.
        catch_to_backend_error(|| unsafe { send_lifecycle_state_uncaught(engine, state) })
    }

    /// Borrows this engine's `FlutterBinaryMessenger`, retained.
    ///
    /// An accessor rather than a public `engine` field: a caller needs the
    /// messenger to register a platform-channel handler (see
    /// [`crate::FlutterSurface`]), and handing out the raw `FlutterEngine*`
    /// instead would let it do anything at all to the engine, including the
    /// lifecycle calls this type exists to sequence.
    ///
    /// The returned handle keeps the *messenger* alive, which is not the same
    /// as keeping the engine alive — on macOS the messenger is a relay that
    /// holds its engine weakly. Sending through it after the engine has shut
    /// down is therefore inert rather than unsound, but callers should still
    /// drop their surfaces before the engine (see [`crate::FlutterSurface`]'s
    /// `Drop`).
    ///
    /// # Errors
    ///
    /// [`BackendError::Unavailable`] if an Objective-C exception was caught.
    pub(crate) fn binary_messenger(&self) -> Result<Retained<AnyObject>, BackendError> {
        let engine = &self.engine;
        catch_to_backend_error(|| {
            // SAFETY: `engine` is `self`'s own live `FlutterEngine*` — `boot`
            // only ever constructs `self` from a successfully initialized
            // engine — and `binaryMessenger` is a read-only property every
            // live `FlutterEngine*` responds to, returning an object
            // conforming to `FlutterBinaryMessenger`. This method requires
            // the caller to be on the main thread, per this type's
            // module-level contract.
            unsafe { msg_send![engine, binaryMessenger] }
        })
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
pub(crate) fn catch_to_backend_error<F, R>(f: F) -> Result<R, BackendError>
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

/// Sends `state`'s raw UTF-8 bytes on `flutter/lifecycle` through `engine`'s
/// `binaryMessenger`. See [`FlutterEngine::set_lifecycle_state`] for why no
/// method-call envelope or codec object is needed here — `StringCodec`
/// payloads are exactly the raw bytes.
///
/// # Safety
///
/// Caller must be on the main thread; `engine` must be a valid, live
/// `FlutterEngine*`.
unsafe fn send_lifecycle_state_uncaught(engine: &AnyObject, state: &str) {
    // SAFETY: `engine` is live per this function's contract;
    // `binaryMessenger` is a read-only property every live `FlutterEngine*`
    // responds to.
    let messenger: Retained<AnyObject> = unsafe { msg_send![engine, binaryMessenger] };
    let channel = NSString::from_str("flutter/lifecycle");
    let payload = NSData::with_bytes(state.as_bytes());
    let message: Option<&NSData> = Some(&payload);
    // SAFETY: `messenger` is the live object just fetched above, which
    // conforms to `FlutterBinaryMessenger`; `sendOnChannel:message:` takes
    // an `NSString*` channel and an `NSData* _Nullable` message (see
    // `FlutterBinaryMessenger.h`) — `channel` and `message` are both live
    // for the duration of this call.
    let _: () = unsafe { msg_send![&messenger, sendOnChannel: &*channel, message: message] };
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
