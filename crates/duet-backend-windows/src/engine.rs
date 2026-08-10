//! Flutter engine and view controller lifetime.
//!
//! Ports the API sequence `spikes/spike-b-windows` proved on this machine:
//!
//! ```text
//! FlutterDesktopEngineCreate      { assets_path, icu_data_path, ... }
//! FlutterDesktopEngineRun(null)                                   -> bool
//! FlutterDesktopViewControllerCreate(width, height, engine)         (once, on first attach)
//!   GetView -> GetHWND, SetParent into the tao window's HWND
//! flutter/lifecycle   MessengerSend  "AppLifecycleState.inactive"
//! flutter/lifecycle   MessengerSend  "AppLifecycleState.hidden"     (before detach)
//! detach = SetParent(view, hidden parking window)                   (controller KEPT)
//! flutter/lifecycle   MessengerSend  "AppLifecycleState.inactive"
//! flutter/lifecycle   MessengerSend  "AppLifecycleState.resumed"    (after attach)
//! FlutterDesktopViewControllerDestroy                               (this IS engine shutdown)
//! ```
//!
//! # Why detach reparents instead of destroying the controller
//!
//! On macOS a `FlutterViewController` can be dropped while its engine lives
//! on. On Windows it cannot: `FlutterDesktopViewControllerCreate` **takes
//! ownership of the engine**, and `FlutterDesktopViewControllerDestroy` shuts
//! the engine down (header contract, and measured by the spike: RSS 258 MB →
//! 78 MB — that is engine shutdown, not view removal; see
//! `spikes/spike-b-windows/FINDINGS.md` W-F1). Duet's `detach` is required to
//! be cheap and reversible, so here it reparents the Flutter view's HWND into
//! a hidden parking window instead (W-F2 verified the full cycle: parked, the
//! engine keeps answering platform-channel traffic; reattached, Dart frame
//! scheduling resumes).
//!
//! The parking window also closes a Win32-specific hazard macOS never had:
//! destroying a Win32 window destroys its children. If a detached view's HWND
//! were left parented to the visible window, `close_window` would take the
//! view down with it behind the live controller's back.
//!
//! The two `flutter/lifecycle` sends around detach are the macOS F1 fix
//! (`crates/duet-backend-macos/FINDINGS.md`), carried over verbatim: `hidden`
//! is what stops Dart's scheduler requesting frames for a surface that has
//! nowhere to composite. The spike confirmed the framework accepts the same
//! transitions on Windows (W-F2) and additionally that the Windows embedder
//! sends lifecycle states of its own on window activation (W-F10) — the
//! manual sends coexisted with them without tripping framework assertions.
//!
//! Every call here must run on the main thread (the engine's platform thread
//! is the thread that created it, and this crate creates it on the thread
//! running the `tao` event loop — see spec §6.2). Callers are responsible for
//! that; this module cannot enforce it itself.

use std::ffi::{CString, OsStr};
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use duet_host::BackendError;
use tao::platform::windows::WindowExtWindows;
use windows_sys::Win32::Foundation::RECT;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, GetClientRect, MoveWindow, SW_SHOW, SetParent, ShowWindow,
    WS_OVERLAPPED,
};

use crate::ffi;

/// Owns one Flutter engine and, at most, one view controller.
///
/// The engine is created and run headless — no `allowHeadlessExecution` analog
/// is needed on Windows; the C API simply composes that way (spike finding
/// W-F3) — and a view controller is created lazily on the first
/// `FlutterEngine::attach`. From then on the controller lives until
/// `FlutterEngine::shutdown`: detach parks its view HWND rather than
/// destroying it, because destroying the controller destroys the engine (see
/// the module docs).
///
/// **One view controller per engine.** The header's ownership contract makes a
/// second `FlutterDesktopViewControllerCreate` against an owned engine
/// structurally wrong (the first create consumed the engine), so this type
/// never creates more than one; `attach` on an engine whose view is already
/// attached to a window is refused in Rust before the C API could be misused.
///
/// This type is `pub` only so that [`crate::FlutterSurface::new`] can name it
/// in its signature (a `pub` function mentioning a `pub(crate)` type trips
/// `private_interfaces`). Every method on it stays `pub(crate)`: the engine's
/// lifecycle is driven exclusively through [`crate::WinBackend`]'s
/// [`duet_host::WindowBackend`] impl, and nothing outside this crate may
/// boot, attach, detach or shut one down directly.
///
/// `!Send`/`!Sync` falls out of the raw pointer fields, which is load-bearing:
/// every method must run on the thread that created the engine.
pub struct FlutterEngine {
    /// The `FlutterDesktopEngineRef`. After the first `attach`, ownership has
    /// passed to `controller` per the C API's contract, and this copy is only
    /// used to reach the messenger.
    engine: ffi::EngineRef,
    /// The engine's messenger, `AddRef`'d at boot so it stays safe to hand to
    /// surfaces and to use in `Drop` paths; `Release`'d in [`Drop`].
    messenger: ffi::MessengerRef,
    /// The view controller and its view's HWND, created on first attach.
    controller: Option<ViewState>,
    /// The hidden parking window detached views are reparented into. Created
    /// at boot (a bare Win32 `STATIC`-class top-level window, never shown), so
    /// detach cannot fail on window creation.
    parking: ffi::HWND,
    /// Whether [`FlutterEngine::shutdown`] has already run, so `Drop` does not
    /// destroy the engine a second time.
    shut_down: bool,
}

/// The view controller once it exists, and where its view currently lives.
struct ViewState {
    controller: ffi::ViewControllerRef,
    /// The Flutter view's child HWND (owned by the controller; this is only a
    /// handle for `SetParent`/`MoveWindow`).
    view_hwnd: ffi::HWND,
    /// Whether the view is currently parented into a caller's window (as
    /// opposed to parked). Detaching when already parked is a no-op.
    attached: bool,
}

impl FlutterEngine {
    /// Boots an engine with no view, from the assets in `data_dir`.
    ///
    /// `data_dir` is the `data` directory a Windows Flutter build leaves next
    /// to the runner exe — `build/windows/x64/runner/Debug/data` — containing
    /// `flutter_assets/` (with `kernel_blob.bin` for the debug/JIT builds this
    /// project exercises) and `icudtl.dat`. Windows has no bundle object like
    /// macOS's `App.framework`; the two paths are passed explicitly.
    ///
    /// # Errors
    ///
    /// [`BackendError::Unavailable`] if the expected files are missing from
    /// `data_dir`, if `FlutterDesktopEngineCreate` returned null, or if
    /// `FlutterDesktopEngineRun` returned false. The run is synchronous
    /// enough that a `true` here means the isolate is genuinely serving
    /// (spike finding W-F4).
    pub(crate) fn boot(data_dir: &str) -> Result<Self, BackendError> {
        // The engine resolves relative paths against the directory containing
        // the EXE, not the process's working directory — so a cwd-relative
        // path would pass the existence checks below and then boot an engine
        // that cannot resolve its kernel binary. Absolutize against the cwd
        // here, once, for every caller. (`canonicalize` yields a `\\?\`
        // verbatim path, which the engine accepts — the spike booted from
        // one.)
        let data = Path::new(data_dir)
            .canonicalize()
            .unwrap_or_else(|_| Path::new(data_dir).to_path_buf());
        let assets = data.join("flutter_assets");
        let icu = data.join("icudtl.dat");
        if !assets.is_dir() {
            return Err(BackendError::Unavailable(format!(
                "flutter_assets not found at {} — is this the data directory of a \
                 `flutter build windows --debug` output?",
                assets.display()
            )));
        }
        if !icu.is_file() {
            return Err(BackendError::Unavailable(format!(
                "icudtl.dat not found at {}",
                icu.display()
            )));
        }

        let assets_w = wide(assets.as_os_str());
        let icu_w = wide(icu.as_os_str());
        let props = ffi::FlutterDesktopEngineProperties {
            assets_path: assets_w.as_ptr(),
            icu_data_path: icu_w.as_ptr(),
            aot_library_path: std::ptr::null(),
            dart_entrypoint: std::ptr::null(),
            dart_entrypoint_argc: 0,
            dart_entrypoint_argv: std::ptr::null(),
            gpu_preference: 0,
            ui_thread_policy: 0,
            accessibility_mode: 0,
            impeller_switch: 0,
        };
        // SAFETY: `props` and the wide strings it points into are live for the
        // duration of the call, which is all the header requires (the struct
        // is deep-copied). Must run on the thread that will pump the engine's
        // messages — the module-level main-thread contract.
        let engine = unsafe { ffi::FlutterDesktopEngineCreate(&props) };
        if engine.is_null() {
            return Err(BackendError::Unavailable(
                "FlutterDesktopEngineCreate returned null".to_string(),
            ));
        }

        // SAFETY: `engine` is the live engine just created; null entrypoint
        // means `main`, per the header.
        let ran = unsafe { ffi::FlutterDesktopEngineRun(engine, std::ptr::null()) };
        if !ran {
            // SAFETY: `engine` was created above, never run, and never handed
            // to a view controller; destroying it here is the documented
            // cleanup for a failed run.
            unsafe { ffi::FlutterDesktopEngineDestroy(engine) };
            return Err(BackendError::Unavailable(
                "FlutterDesktopEngineRun(null) returned false".to_string(),
            ));
        }

        // SAFETY: `engine` is live and running. `GetMessenger` does not confer
        // ownership; the `AddRef` immediately after does, so the ref stays
        // valid for surfaces and Drop paths even after engine shutdown (sends
        // through it after shutdown are made inert by the `IsAvailable` check
        // in `messenger_send` rather than being use-after-free).
        let messenger = unsafe { ffi::FlutterDesktopEngineGetMessenger(engine) };
        if messenger.is_null() {
            // SAFETY: same failed-boot cleanup as above.
            unsafe { ffi::FlutterDesktopEngineDestroy(engine) };
            return Err(BackendError::Unavailable(
                "FlutterDesktopEngineGetMessenger returned null".to_string(),
            ));
        }
        // SAFETY: `messenger` is the live messenger just fetched.
        unsafe { ffi::FlutterDesktopMessengerAddRef(messenger) };

        let parking = create_parking_window()?;

        Ok(FlutterEngine {
            engine,
            messenger,
            controller: None,
            parking,
            shut_down: false,
        })
    }

    /// Parents the Flutter view into `window`'s client area, then restarts
    /// Dart's frame scheduling.
    ///
    /// On the first call this creates the view controller (which, per the C
    /// API's contract, takes ownership of the engine — see the module docs);
    /// on later calls it reparents the parked view HWND back out of the
    /// parking window. Either way the view is sized to the window's client
    /// rect, shown, and focused, and then `AppLifecycleState.inactive` and
    /// `AppLifecycleState.resumed` are sent on `flutter/lifecycle`. The
    /// intermediate `inactive` step is not optional: the framework's
    /// transition table (`packages/flutter/lib/src/services/binding.dart`)
    /// only allows `hidden -> inactive -> resumed` — sending `resumed`
    /// directly from `hidden` trips a framework assertion. See
    /// [`FlutterEngine::detach`] for the matching half.
    ///
    /// # Errors
    ///
    /// [`BackendError::Unavailable`] if a view is already attached (callers
    /// must [`FlutterEngine::detach`] first), if the view controller or its
    /// HWND could not be created, or if either lifecycle message failed to
    /// send (the view is attached either way at that point — only the
    /// frame-scheduling restart is in question).
    pub(crate) fn attach(&mut self, window: &tao::window::Window) -> Result<(), BackendError> {
        if self.controller.as_ref().is_some_and(|c| c.attached) {
            return Err(BackendError::Unavailable(
                "a view is already attached to this engine — detach it first".to_string(),
            ));
        }
        let parent = window.hwnd() as ffi::HWND;
        if parent.is_null() {
            return Err(BackendError::Unavailable(
                "tao window has no backing HWND".to_string(),
            ));
        }

        match &mut self.controller {
            Some(view) => {
                // Reattach: pull the parked view back out of the parking
                // window. SAFETY: `view_hwnd` is the controller's live view
                // window (the controller is never destroyed outside
                // `shutdown`), `parent` was checked non-null above, and all
                // these Win32 calls are on the window's owning thread per the
                // module contract.
                unsafe { reparent_into(view.view_hwnd, parent) };
                view.attached = true;
            }
            None => {
                let size = window.inner_size();
                // SAFETY: `self.engine` is live and running (boot succeeded),
                // and no controller has ever been created for it — this arm is
                // the `None` case. Per the header this passes engine ownership
                // to the controller.
                let controller = unsafe {
                    ffi::FlutterDesktopViewControllerCreate(
                        size.width as i32,
                        size.height as i32,
                        self.engine,
                    )
                };
                if controller.is_null() {
                    return Err(BackendError::Unavailable(
                        "FlutterDesktopViewControllerCreate returned null".to_string(),
                    ));
                }
                // SAFETY: `controller` is the live controller just created.
                let view = unsafe { ffi::FlutterDesktopViewControllerGetView(controller) };
                let view_hwnd = if view.is_null() {
                    std::ptr::null_mut()
                } else {
                    // SAFETY: `view` is the controller's live view.
                    unsafe { ffi::FlutterDesktopViewGetHWND(view) }
                };
                if view_hwnd.is_null() {
                    // SAFETY: `controller` is live and owns the engine; this
                    // is the documented cleanup, and `shut_down` is set so no
                    // second destroy is attempted later.
                    unsafe { ffi::FlutterDesktopViewControllerDestroy(controller) };
                    self.shut_down = true;
                    return Err(BackendError::Unavailable(
                        "Flutter view controller produced no view HWND".to_string(),
                    ));
                }
                // SAFETY: both handles checked non-null; owning thread per the
                // module contract.
                unsafe { reparent_into(view_hwnd, parent) };
                self.controller = Some(ViewState {
                    controller,
                    view_hwnd,
                    attached: true,
                });
            }
        }

        self.set_lifecycle_state("AppLifecycleState.inactive")?;
        self.set_lifecycle_state("AppLifecycleState.resumed")?;
        Ok(())
    }

    /// Stops Dart's frame scheduling, then parks the view.
    ///
    /// The spike measured that parking reclaims essentially nothing (RSS flat
    /// around 255 MB) — only [`FlutterEngine::shutdown`] frees memory. The
    /// point of `detach` is to keep the engine alive so a later
    /// [`FlutterEngine::attach`] skips the engine boot entirely (W-F2: a
    /// parked engine kept answering platform-channel traffic and resumed
    /// rendering on reattach).
    ///
    /// **This carries the macOS F1 fix.** Before the view is reparented, this
    /// sends `AppLifecycleState.inactive` then `AppLifecycleState.hidden` on
    /// `flutter/lifecycle`. `hidden` is what
    /// `SchedulerBinding.handleAppLifecycleStateChanged` uses to disable
    /// frame scheduling; without it, any app with a running `Ticker` keeps
    /// requesting a frame every vsync for a view with nowhere to composite —
    /// on macOS that produced 162,686 backing-store errors in 90 seconds. The
    /// `inactive` step is not optional, and must come *before* `hidden`: the
    /// framework's transition table only allows `resumed -> inactive ->
    /// hidden`, and a future edit that "simplifies" this to a single `hidden`
    /// send will trip a framework assertion, not just lose the
    /// frame-stopping effect.
    ///
    /// # Errors
    ///
    /// [`BackendError::Unavailable`] if either lifecycle send failed. The
    /// reparent itself still runs — the view is parked either way — but the
    /// caller genuinely needs to know the frame-storm prevention may not have
    /// taken effect this time. Detaching with no view attached (or no
    /// controller yet) is a no-op, not an error.
    pub(crate) fn detach(&mut self) -> Result<(), BackendError> {
        let parking = self.parking;
        let Some(view) = self.controller.as_mut().filter(|c| c.attached) else {
            return Ok(());
        };
        let lifecycle_result = send_lifecycle(self.messenger, "AppLifecycleState.inactive")
            .and_then(|()| send_lifecycle(self.messenger, "AppLifecycleState.hidden"));
        // SAFETY: `view_hwnd` is the controller's live view window and
        // `parking` was created at boot and lives until shutdown; owning
        // thread per the module contract.
        unsafe { SetParent(view.view_hwnd, parking) };
        view.attached = false;
        lifecycle_result
    }

    /// Shuts the engine down. **This is what reclaims memory** — the spike
    /// measured 258 MB before and 78 MB after.
    ///
    /// Detaches any still-attached view first, so `shutdown` is safe to call
    /// without a preceding `detach`. Idempotent: a second call is a no-op.
    ///
    /// Infallible by design, for the same reason as the macOS backend's
    /// `shutdown`: a failed lifecycle send during the detach only means the
    /// frame storm might still be running when the engine is destroyed, and
    /// the destroy runs regardless — there is no different action `shutdown`
    /// could take with the error, since the handle is dropped immediately
    /// after either way.
    pub(crate) fn shutdown(&mut self) {
        let _ = self.detach();
        if self.shut_down {
            return;
        }
        self.shut_down = true;
        match self.controller.take() {
            // SAFETY: `controller` is live (only `shutdown` destroys it, and
            // the `shut_down` flag above makes this path run once). Per the
            // header, destroying the controller shuts down the engine it owns.
            Some(view) => unsafe { ffi::FlutterDesktopViewControllerDestroy(view.controller) },
            // SAFETY: no controller was ever created, so `self.engine` still
            // owns itself and `FlutterDesktopEngineDestroy` is the documented
            // teardown for it.
            None => {
                let _ = unsafe { ffi::FlutterDesktopEngineDestroy(self.engine) };
            }
        }
        // SAFETY: `parking` is the window created at boot; destroying a
        // window on its owning thread is always sound, and any still-parked
        // view HWND was owned by the controller destroyed above.
        unsafe { DestroyWindow(self.parking) };
    }

    /// Sends `state` as a raw UTF-8 message on the `flutter/lifecycle`
    /// channel, through this engine's messenger.
    ///
    /// `flutter/lifecycle` is a `BasicMessageChannel<String?>` using
    /// `StringCodec`, and `StringCodec` puts the payload on the wire as-is —
    /// raw UTF-8 bytes, no method-call envelope, no length prefix. So this is
    /// one `FlutterDesktopMessengerSend`, the same primitive
    /// [`crate::FlutterSurface`] pushes through for `duet/rpc`.
    ///
    /// Expected callers pass one of `"AppLifecycleState.inactive"`,
    /// `"AppLifecycleState.hidden"`, or `"AppLifecycleState.resumed"` — see
    /// [`FlutterEngine::detach`] and [`FlutterEngine::attach`] for why those
    /// three specifically, and why the order between them matters.
    ///
    /// # Errors
    ///
    /// [`BackendError::Unavailable`] if the messenger refused the send — for
    /// example, an engine that has already shut down.
    pub(crate) fn set_lifecycle_state(&self, state: &str) -> Result<(), BackendError> {
        send_lifecycle(self.messenger, state)
    }

    /// Borrows this engine's messenger for a [`crate::FlutterSurface`].
    ///
    /// An accessor rather than a public field: a caller needs the messenger to
    /// register a platform-channel handler, and handing out the raw
    /// `FlutterDesktopEngineRef` instead would let it do anything at all to
    /// the engine, including the lifecycle calls this type exists to
    /// sequence.
    ///
    /// The returned handle `AddRef`s the messenger, which keeps the
    /// *messenger* alive, not the engine. Sending through it after the engine
    /// has shut down is inert rather than unsound — every send goes through
    /// `FlutterDesktopMessengerLock`/`IsAvailable` — but callers should still
    /// drop their surfaces before the engine (see [`crate::FlutterSurface`]'s
    /// `Drop`).
    ///
    /// # Errors
    ///
    /// Infallible today (the messenger was validated non-null at boot);
    /// returns `Result` to mirror the macOS accessor's shape so surface code
    /// stays diff-comparable between the two backends.
    pub(crate) fn binary_messenger(&self) -> Result<Messenger, BackendError> {
        // SAFETY: `self.messenger` is the live, AddRef'd messenger from boot.
        Ok(unsafe { Messenger::add_ref(self.messenger) })
    }
}

impl Drop for FlutterEngine {
    /// Guarantees the engine is destroyed even if a caller forgets to call
    /// `FlutterEngine::shutdown` explicitly (for example on an early
    /// `?`-propagated error path elsewhere in the backend) — an engine leaked
    /// past its handle would otherwise never reclaim its 100+ MB. Also
    /// releases the messenger ref taken at boot.
    fn drop(&mut self) {
        if !self.shut_down {
            self.shutdown();
        }
        // SAFETY: `self.messenger` was AddRef'd once at boot and never
        // released before this point; Release is documented thread-safe.
        unsafe { ffi::FlutterDesktopMessengerRelease(self.messenger) };
    }
}

/// An `AddRef`'d `FlutterDesktopMessengerRef` whose sends are guarded.
///
/// The Windows messenger has an explicit lifetime API — `AddRef`/`Release`,
/// `Lock`/`Unlock`, `IsAvailable` — instead of macOS's weakly-held relay.
/// This wrapper owns one reference and routes every send through
/// [`Messenger::send`], which locks and checks availability first, so a send
/// after engine shutdown is a clean `false` rather than a use-after-free.
///
/// `pub` for the same `private_interfaces` reason as [`FlutterEngine`]; every
/// method is `pub(crate)`. `!Send`/`!Sync` via the raw pointer, which is
/// correct: the surface that holds this is main-thread-only.
pub struct Messenger {
    raw: ffi::MessengerRef,
}

impl Messenger {
    /// Takes a new reference on `raw`.
    ///
    /// # Safety
    ///
    /// `raw` must be a valid `FlutterDesktopMessengerRef` (alive or already
    /// disconnected from its engine — both are fine, the ref-count API is
    /// documented thread-safe and shutdown-safe).
    pub(crate) unsafe fn add_ref(raw: ffi::MessengerRef) -> Self {
        // SAFETY: delegated to this function's contract.
        unsafe { ffi::FlutterDesktopMessengerAddRef(raw) };
        Messenger { raw }
    }

    /// The raw ref, for registration calls that need it.
    pub(crate) fn raw(&self) -> ffi::MessengerRef {
        self.raw
    }

    /// Sends `bytes` on `channel` if the engine is still available.
    ///
    /// Returns whether the send happened. Locks around the availability check
    /// per the header's contract, so the engine cannot disappear between the
    /// check and the send.
    pub(crate) fn send(&self, channel: &CString, bytes: &[u8]) -> bool {
        // SAFETY: `self.raw` holds a reference taken by `add_ref`, so the
        // messenger object outlives this call; Lock/IsAvailable/Unlock are
        // documented thread-safe, and the send only runs while the lock
        // guarantees a live engine.
        unsafe {
            ffi::FlutterDesktopMessengerLock(self.raw);
            let available = ffi::FlutterDesktopMessengerIsAvailable(self.raw);
            let sent = if available {
                ffi::FlutterDesktopMessengerSend(
                    self.raw,
                    channel.as_ptr(),
                    bytes.as_ptr(),
                    bytes.len(),
                )
            } else {
                false
            };
            ffi::FlutterDesktopMessengerUnlock(self.raw);
            sent
        }
    }
}

impl Drop for Messenger {
    fn drop(&mut self) {
        // SAFETY: this wrapper took exactly one reference in `add_ref`.
        unsafe { ffi::FlutterDesktopMessengerRelease(self.raw) };
    }
}

/// Null-terminated UTF-16 for the engine properties' wide-string paths.
fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

/// Sends a lifecycle string through a raw messenger ref (used both by
/// [`FlutterEngine::set_lifecycle_state`] and internally by `detach`, which
/// holds `&mut self` and so cannot also borrow a [`Messenger`]).
fn send_lifecycle(messenger: ffi::MessengerRef, state: &str) -> Result<(), BackendError> {
    let channel = CString::new("flutter/lifecycle").expect("channel name has no interior NUL");
    // SAFETY: `messenger` is the engine's AddRef'd messenger (see
    // `FlutterEngine::boot`); Lock/IsAvailable guard the send against a
    // shut-down engine exactly as in `Messenger::send`.
    let sent = unsafe {
        ffi::FlutterDesktopMessengerLock(messenger);
        let available = ffi::FlutterDesktopMessengerIsAvailable(messenger);
        let sent = if available {
            ffi::FlutterDesktopMessengerSend(
                messenger,
                channel.as_ptr(),
                state.as_ptr(),
                state.len(),
            )
        } else {
            false
        };
        ffi::FlutterDesktopMessengerUnlock(messenger);
        sent
    };
    if sent {
        Ok(())
    } else {
        Err(BackendError::Unavailable(format!(
            "flutter/lifecycle send of {state:?} was refused — engine shut down?"
        )))
    }
}

/// Creates the hidden parking window detached views are reparented into: a
/// bare `STATIC`-class top-level window, 1×1, never shown. A real window
/// rather than a message-only one, because the spike validated parking under
/// a real (hidden) window and a message-only parent's rendering behavior for
/// a composited child is undocumented.
fn create_parking_window() -> Result<ffi::HWND, BackendError> {
    let class: Vec<u16> = OsStr::new("STATIC")
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let title: Vec<u16> = OsStr::new("duet parking")
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: plain window creation with a predefined class; all pointers are
    // live for the call. No WS_VISIBLE, so it never appears.
    let hwnd = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPED,
            0,
            0,
            1,
            1,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null(),
        )
    };
    if hwnd.is_null() {
        return Err(BackendError::Unavailable(
            "failed to create the hidden parking window".to_string(),
        ));
    }
    Ok(hwnd)
}

/// Parents `child` into `parent`, fills the client rect, shows and focuses it
/// — the same sequence the stock Flutter runner's `SetChildContent` performs.
///
/// # Safety
///
/// Caller must be on the windows' owning thread; both handles must be live.
unsafe fn reparent_into(child: ffi::HWND, parent: ffi::HWND) {
    // SAFETY: delegated to this function's contract on both handles.
    unsafe {
        SetParent(child, parent);
        let mut rect = std::mem::zeroed::<RECT>();
        GetClientRect(parent, &mut rect);
        MoveWindow(
            child,
            rect.left,
            rect.top,
            rect.right - rect.left,
            rect.bottom - rect.top,
            1,
        );
        ShowWindow(child, SW_SHOW);
        SetFocus(child);
    }
}
