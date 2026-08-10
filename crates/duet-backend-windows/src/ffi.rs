//! The subset of `flutter_windows.h` this crate calls, declared by hand.
//!
//! The Flutter Windows embedder is a plain C API — opaque pointers, booleans,
//! function pointers with a `user_data` slot. That is the whole reason
//! docs/10-porting.md predicted "Windows should be easier than macOS was": no
//! `objc2`, no `block2`, no exception bridging. The API surface this backend
//! needs is small enough that hand-written declarations, checked against the
//! header shipped in the engine artifact cache
//! (`<flutter>/bin/cache/artifacts/engine/windows-x64/flutter_windows.h`),
//! beat a bindgen build step.
//!
//! Everything here is `pub(crate)`: the typed lifecycle lives in
//! [`crate::engine::FlutterEngine`] and the channel plumbing in
//! [`crate::FlutterSurface`]; nothing outside this crate may speak to the
//! engine directly.
//!
//! # Ownership facts that shape the whole backend
//!
//! Two lines of the header are load-bearing for everything above this module
//! (verified live by `spikes/spike-b-windows`, not just read):
//!
//! - `FlutterDesktopViewControllerCreate` "**takes ownership of |engine|**, so
//!   `FlutterDesktopEngineDestroy` should no longer be called on it, as it
//!   will be called internally when the view controller is destroyed."
//! - `FlutterDesktopViewControllerDestroy` "**shuts down the engine instance**
//!   associated with |controller|."
//!
//! So unlike macOS — where a `FlutterViewController` can be dropped while the
//! engine lives on — destroying the Windows view controller **is** engine
//! shutdown. Duet's cheap-and-reversible `detach` therefore cannot destroy
//! the controller; it reparents the view's HWND into a hidden parking window
//! instead (see [`crate::engine::FlutterEngine::detach`]).

use std::ffi::{c_char, c_void};

/// A Win32 window handle, as the raw pointer `windows-sys` also uses. The
/// all-caps Win32 spelling is kept deliberately — it names the platform type,
/// not an acronym this crate coined.
#[allow(clippy::upper_case_acronyms)]
pub(crate) type HWND = *mut c_void;
/// Opaque `FlutterDesktopEngineRef`.
pub(crate) type EngineRef = *mut c_void;
/// Opaque `FlutterDesktopViewControllerRef`.
pub(crate) type ViewControllerRef = *mut c_void;
/// Opaque `FlutterDesktopViewRef`.
pub(crate) type ViewRef = *mut c_void;
/// Opaque `FlutterDesktopMessengerRef`.
pub(crate) type MessengerRef = *mut c_void;
/// Opaque `const FlutterDesktopMessageResponseHandle*` — single-use; see
/// [`FlutterDesktopMessengerSendResponse`].
pub(crate) type ResponseHandle = *const c_void;

/// `FlutterDesktopEngineProperties` (flutter_windows.h): how an engine finds
/// its Dart program. Paths are wide (UTF-16) strings; the struct only needs to
/// outlive the `FlutterDesktopEngineCreate` call, which deep-copies it.
#[repr(C)]
pub(crate) struct FlutterDesktopEngineProperties {
    /// Path to the app's `flutter_assets` directory.
    pub assets_path: *const u16,
    /// Path to `icudtl.dat`.
    pub icu_data_path: *const u16,
    /// Path to the AOT library, ignored for debug/JIT builds (may be null).
    pub aot_library_path: *const u16,
    /// Top-level Dart entrypoint name; null means `main`.
    pub dart_entrypoint: *const c_char,
    /// Number of entrypoint arguments.
    pub dart_entrypoint_argc: i32,
    /// Entrypoint arguments, deep-copied during create.
    pub dart_entrypoint_argv: *const *const c_char,
    /// `FlutterDesktopGpuPreference`; 0 = NoPreference.
    pub gpu_preference: i32,
    /// `FlutterDesktopUIThreadPolicy`; 0 = Default.
    pub ui_thread_policy: i32,
    /// `FlutterDesktopAccessibilityMode`; 0 = DefaultAccessibilityMode.
    pub accessibility_mode: i32,
    /// `FlutterDesktopImpellerSwitch`; 0 = DefaultImpeller.
    pub impeller_switch: i32,
}

/// `FlutterDesktopMessage` (flutter_messenger.h): one inbound platform-channel
/// message. `message` borrows engine-owned bytes valid only for the duration
/// of the handler call; `response_handle`, when non-null, must be answered
/// exactly once via [`FlutterDesktopMessengerSendResponse`].
#[repr(C)]
pub(crate) struct FlutterDesktopMessage {
    /// Size of this struct as created by Flutter.
    pub struct_size: usize,
    /// The channel the message arrived on.
    pub channel: *const c_char,
    /// The raw message bytes (borrowed; may be null for an empty payload).
    pub message: *const u8,
    /// The length of `message`.
    pub message_size: usize,
    /// The single-use response handle, or null if no reply is expected.
    pub response_handle: ResponseHandle,
}

/// `FlutterDesktopMessageCallback`: the C function pointer a channel handler
/// registers. Invoked with the messenger, the message, and the registration's
/// `user_data`.
pub(crate) type MessageCallback = Option<
    unsafe extern "C" fn(
        messenger: MessengerRef,
        message: *const FlutterDesktopMessage,
        user_data: *mut c_void,
    ),
>;

#[link(name = "flutter_windows.dll")]
unsafe extern "C" {
    pub(crate) fn FlutterDesktopEngineCreate(
        engine_properties: *const FlutterDesktopEngineProperties,
    ) -> EngineRef;
    pub(crate) fn FlutterDesktopEngineDestroy(engine: EngineRef) -> bool;
    pub(crate) fn FlutterDesktopEngineRun(engine: EngineRef, entry_point: *const c_char) -> bool;
    pub(crate) fn FlutterDesktopEngineGetMessenger(engine: EngineRef) -> MessengerRef;

    pub(crate) fn FlutterDesktopViewControllerCreate(
        width: i32,
        height: i32,
        engine: EngineRef,
    ) -> ViewControllerRef;
    pub(crate) fn FlutterDesktopViewControllerDestroy(controller: ViewControllerRef);
    pub(crate) fn FlutterDesktopViewControllerGetView(controller: ViewControllerRef) -> ViewRef;
    pub(crate) fn FlutterDesktopViewGetHWND(view: ViewRef) -> HWND;

    pub(crate) fn FlutterDesktopMessengerSend(
        messenger: MessengerRef,
        channel: *const c_char,
        message: *const u8,
        message_size: usize,
    ) -> bool;
    pub(crate) fn FlutterDesktopMessengerSendResponse(
        messenger: MessengerRef,
        handle: ResponseHandle,
        data: *const u8,
        data_length: usize,
    );
    pub(crate) fn FlutterDesktopMessengerSetCallback(
        messenger: MessengerRef,
        channel: *const c_char,
        callback: MessageCallback,
        user_data: *mut c_void,
    );
    pub(crate) fn FlutterDesktopMessengerAddRef(messenger: MessengerRef) -> MessengerRef;
    pub(crate) fn FlutterDesktopMessengerRelease(messenger: MessengerRef);
    pub(crate) fn FlutterDesktopMessengerIsAvailable(messenger: MessengerRef) -> bool;
    pub(crate) fn FlutterDesktopMessengerLock(messenger: MessengerRef) -> MessengerRef;
    pub(crate) fn FlutterDesktopMessengerUnlock(messenger: MessengerRef);
}
