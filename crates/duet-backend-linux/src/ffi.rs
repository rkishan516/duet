//! The subset of the `flutter_linux` API this crate calls, declared by hand.
//!
//! The Flutter Linux embedder is a GObject C API. The functions below are the
//! whole surface this backend needs, checked against the headers shipped in
//! the engine artifact cache
//! (`<flutter>/bin/cache/artifacts/engine/linux-x64/flutter_linux/*.h`) and
//! exercised live by `spikes/spike-b-linux`. Hand-written declarations beat a
//! bindgen step at this size, exactly as they did for `flutter_windows.h`.
//!
//! Everything here is `pub(crate)`: the typed lifecycle lives in
//! [`crate::engine::FlutterEngine`] and the channel plumbing in
//! [`crate::FlutterSurface`]; nothing outside this crate may speak to the
//! engine directly.
//!
//! # The facts that shape the whole backend (spike findings L-F1..L-F3)
//!
//! - **There is no public `fl_engine_start`.** The engine starts inside the
//!   *implicit* view's realize path, so boot IS view creation:
//!   `fl_view_new(project)` packed into a window and realized.
//!   `fl_view_new_for_engine` makes a *secondary* view and is rejected on an
//!   unstarted engine.
//! - **Re-realizing the implicit view is fatal** — it re-runs the engine
//!   start against a live engine and wrecks the handle. The view therefore
//!   never moves and is never unrealized until the renderer is destroyed;
//!   attach/detach is mapping and unmapping its window.
//! - `FlEngine` and `FlView` are ordinary GObjects: lifetime is reference
//!   counting, and the embedder's engine shutdown runs in `FlEngine`'s
//!   dispose — dropping the last reference is what stops Dart and reclaims
//!   the memory.

use std::ffi::{c_char, c_void};

/// Opaque `FlDartProject*`.
pub(crate) type FlDartProjectRef = *mut c_void;
/// Opaque `FlEngine*`.
pub(crate) type FlEngineRef = *mut c_void;
/// Opaque `FlView*` — also a `GtkWidget*`.
pub(crate) type FlViewRef = *mut c_void;
/// Opaque `FlBinaryMessenger*`.
pub(crate) type MessengerRef = *mut c_void;
/// Opaque `FlBinaryMessengerResponseHandle*` — single-use; see
/// [`fl_binary_messenger_send_response`].
pub(crate) type ResponseHandleRef = *mut c_void;
/// `GBytes*`, from the glib this crate already links through gtk-rs.
pub(crate) type GBytesRef = *mut glib_sys::GBytes;

/// `FlBinaryMessengerMessageHandler` (fl_binary_messenger.h).
pub(crate) type MessageHandler = Option<
    unsafe extern "C" fn(
        messenger: MessengerRef,
        channel: *const c_char,
        message: GBytesRef,
        response_handle: ResponseHandleRef,
        user_data: *mut c_void,
    ),
>;

/// `GAsyncReadyCallback`, as `fl_binary_messenger_send_on_channel` takes it.
pub(crate) type AsyncReady = Option<
    unsafe extern "C" fn(
        source: *mut gobject_sys::GObject,
        result: *mut c_void,
        user_data: *mut c_void,
    ),
>;

#[link(name = "flutter_linux_gtk")]
unsafe extern "C" {
    pub(crate) fn fl_dart_project_new() -> FlDartProjectRef;
    pub(crate) fn fl_dart_project_set_assets_path(project: FlDartProjectRef, path: *mut c_char);
    pub(crate) fn fl_dart_project_set_icu_data_path(project: FlDartProjectRef, path: *mut c_char);

    pub(crate) fn fl_view_new(project: FlDartProjectRef) -> FlViewRef;
    pub(crate) fn fl_view_get_engine(view: FlViewRef) -> FlEngineRef;

    pub(crate) fn fl_engine_get_binary_messenger(engine: FlEngineRef) -> MessengerRef;

    pub(crate) fn fl_binary_messenger_set_message_handler_on_channel(
        messenger: MessengerRef,
        channel: *const c_char,
        handler: MessageHandler,
        user_data: *mut c_void,
        destroy_notify: Option<unsafe extern "C" fn(*mut c_void)>,
    );
    pub(crate) fn fl_binary_messenger_send_response(
        messenger: MessengerRef,
        response_handle: ResponseHandleRef,
        response: GBytesRef,
        error: *mut *mut glib_sys::GError,
    ) -> glib_sys::gboolean;
    pub(crate) fn fl_binary_messenger_send_on_channel(
        messenger: MessengerRef,
        channel: *const c_char,
        message: GBytesRef,
        cancellable: *mut c_void,
        callback: AsyncReady,
        user_data: *mut c_void,
    );
}

/// Disables Impeller on `project`, if this engine has the switch. Returns
/// whether it did.
///
/// `fl_dart_project_set_enable_impeller` is looked up with `dlsym` rather
/// than linked, deliberately: the symbol only exists in newer engines (the
/// pinned master toolchain this port was built against has it; the stable
/// channel's engine — the one CI links — does not, and an eager import made
/// every test and example binary fail to link there). The fallback is not a
/// degradation: an engine old enough to lack the opt-out predates the
/// Impeller-by-default flip on GTK that made the opt-out necessary (L-F4),
/// so on such engines there is nothing to disable.
pub(crate) fn dart_project_disable_impeller(project: FlDartProjectRef) -> bool {
    type SetEnableImpeller =
        unsafe extern "C" fn(project: FlDartProjectRef, enable: glib_sys::gboolean);
    // SAFETY: RTLD_DEFAULT searches the already-loaded images —
    // libflutter_linux_gtk.so is a DT_NEEDED dependency of this binary, so it
    // is resolved before any Rust code runs. The transmute matches the C
    // signature declared in fl_dart_project.h, and a null lookup is checked
    // before the call.
    unsafe {
        let symbol = libc::dlsym(
            libc::RTLD_DEFAULT,
            c"fl_dart_project_set_enable_impeller".as_ptr(),
        );
        if symbol.is_null() {
            return false;
        }
        let set_enable_impeller: SetEnableImpeller = std::mem::transmute(symbol);
        set_enable_impeller(project, 0);
        true
    }
}

/// A new `GBytes` copying `data`. The caller owns the returned reference.
pub(crate) fn bytes_new(data: &[u8]) -> GBytesRef {
    // SAFETY: g_bytes_new copies the buffer; the slice is live for the call.
    unsafe { glib_sys::g_bytes_new(data.as_ptr() as *const c_void, data.len()) }
}

/// Copies a `GBytes`'s contents out. Accepts null as empty — the embedder
/// hands a null `GBytes*` for an empty payload.
pub(crate) fn bytes_to_vec(bytes: GBytesRef) -> Vec<u8> {
    if bytes.is_null() {
        return Vec::new();
    }
    // SAFETY: `bytes` is a live GBytes per the caller; `g_bytes_get_data`
    // returns a pointer valid while the GBytes lives, and the copy happens
    // immediately.
    unsafe {
        let mut size: usize = 0;
        let data = glib_sys::g_bytes_get_data(bytes, &mut size);
        if data.is_null() {
            Vec::new()
        } else {
            std::slice::from_raw_parts(data as *const u8, size).to_vec()
        }
    }
}

/// The length of a `GBytes` without copying, for size caps. Null is zero.
pub(crate) fn bytes_len(bytes: GBytesRef) -> usize {
    if bytes.is_null() {
        return 0;
    }
    // SAFETY: live GBytes per the caller.
    unsafe { glib_sys::g_bytes_get_size(bytes) }
}
