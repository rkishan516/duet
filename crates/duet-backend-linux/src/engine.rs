//! Flutter engine and view lifetime.
//!
//! Ports the sequence `spikes/spike-b-linux` proved on this machine:
//!
//! ```text
//! fl_dart_project_new + set_assets_path/set_icu_data_path (+ Impeller off)
//! fl_view_new(project)                       — the IMPLICIT view; engine born with it
//!   pack into the (invisible) window's vbox
//!   realize the widget tree, parents-first   — this IS the engine start (L-F1/L-F2)
//! flutter/lifecycle  send  "AppLifecycleState.inactive"
//! flutter/lifecycle  send  "AppLifecycleState.hidden"     (before detach)
//! detach = gtk_widget_hide(window)           — unmap; realization untouched
//! flutter/lifecycle  send  "AppLifecycleState.inactive"
//! flutter/lifecycle  send  "AppLifecycleState.resumed"    (after attach)
//! attach = gtk_widget_show(window)
//! destroy = gtk_widget_destroy(view) + drop the engine reference
//! ```
//!
//! # Why attach/detach is show/hide and nothing else
//!
//! On macOS the view moves in and out of the window; on Windows it parks in
//! a hidden sibling window. On Linux **the view never moves**: removing a
//! realized `FlView` from its container unrealizes it, and re-realizing the
//! implicit view re-runs the engine start against a live engine — which
//! wrecks the engine handle and floods the main loop with
//! `FlutterEngineRunTask` failures (spike finding L-F3, measured twice). GTK
//! `hide` unmaps without unrealizing, so mapping and unmapping the window
//! the view has lived in since boot is the entire attach/detach story. The
//! F1 lifecycle sends bracket it exactly as on the other platforms.
//!
//! # Why boot needs the window
//!
//! There is no public `fl_engine_start`; the engine starts when the implicit
//! view's renderer realizes (L-F1). Realization needs a real toplevel, so
//! [`FlutterEngine::boot`] realizes the view inside the surface's own
//! (still-invisible) window when one is open, or inside a private hidden
//! holding window when none is — the headless boot the state-sharing
//! examples use. A headless-booted engine can never be attached later
//! (attaching would mean moving the view, which L-F3 forbids); that is a
//! documented Linux constraint, and every current driver already opens the
//! window before `start_renderer`.
//!
//! # Who reclaims the memory
//!
//! `FlEngine` is a GObject, and the embedder's shutdown runs in its
//! *dispose* — but waiting for the last reference to drop does not get
//! there: one embedder-internal reference outlives even the view's
//! destruction (measured here: ref_count 2 after `gtk_widget_destroy`, and
//! a teardown that only dropped its own reference reclaimed 5.4% of the
//! engine's cost in the lifecycle probe instead of most of it). So
//! [`FlutterEngine::shutdown`] *forces* the dispose with
//! `g_object_run_dispose` — the same idiom `gtk_widget_destroy` applies to
//! widgets — which runs `FlutterEngineShutdown`, stops Dart, and reclaims
//! the memory on the spot. Surfaces still hold their own references (engine
//! and messenger objects both), which keeps a surface's send path
//! memory-safe even past `destroy_renderer` — a send there degrades to a
//! no-op through the messenger's internal weak engine reference — but the
//! documented drop-surfaces-first order remains load-bearing for the
//! channel handler's `user_data` (see [`crate::FlutterSurface`]'s `Drop`).
//!
//! Every call here must run on the main thread (the GTK main context).
//! Callers are responsible for that; this module cannot enforce it itself.

use std::ffi::{CString, c_char};
use std::path::Path;

use duet_host::BackendError;
use glib::translate::FromGlibPtrNone;
use gtk::prelude::*;

use crate::ffi;

/// Owns one Flutter engine and the implicit view it was born with.
///
/// This type is `pub` only so that [`crate::FlutterSurface::new`] can name it
/// in its signature (a `pub` function mentioning a `pub(crate)` type trips
/// `private_interfaces`). Every method on it stays `pub(crate)`: the engine's
/// lifecycle is driven exclusively through [`crate::LinuxBackend`]'s
/// [`duet_host::WindowBackend`] impl, and nothing outside this crate may
/// boot, attach, detach or shut one down directly.
///
/// `!Send`/`!Sync` falls out of the raw pointers and gtk-rs handles, which is
/// load-bearing: every method must run on the GTK main context's thread.
pub struct FlutterEngine {
    /// The `FlEngine*`, with one reference this struct owns.
    engine: ffi::FlEngineRef,
    /// The implicit `FlView*`, kept alive by its container; destroyed in
    /// [`FlutterEngine::shutdown`].
    view: ffi::FlViewRef,
    /// The engine's messenger (borrowed from the engine; valid while the
    /// engine object lives, which this struct's reference guarantees).
    messenger: ffi::MessengerRef,
    /// The toplevel the view lives in — the surface's own window, or the
    /// private holding window of a headless boot. Shown by `attach`, hidden
    /// by `detach`.
    window: gtk::Window,
    /// Whether `window` is the surface's real window (attachable) or a
    /// private holding window (headless — never attachable, per L-F3).
    attachable: bool,
    /// Whether the window is currently mapped by `attach`.
    attached: bool,
    /// Whether [`FlutterEngine::shutdown`] has already run.
    shut_down: bool,
}

/// Where [`FlutterEngine::boot`] realizes the view.
pub(crate) enum BootTarget {
    /// The surface's own (invisible) window: attach/detach map and unmap it.
    Window(gtk::Window),
    /// No window is open for the surface: boot into a private hidden holding
    /// window. The engine serves the store but can never be attached.
    Headless,
}

impl FlutterEngine {
    /// Boots an engine from the assets in `data_dir`, realizing its implicit
    /// view into `target`.
    ///
    /// `data_dir` is the `data` directory a Linux Flutter build leaves in its
    /// bundle — `build/linux/x64/debug/bundle/data` — containing
    /// `flutter_assets/` and `icudtl.dat`.
    ///
    /// Impeller is disabled on the project — where the engine offers the
    /// switch; it is resolved at runtime, and an engine without the symbol
    /// predates the Impeller-on-GTK default it opts out of, so its absence
    /// means there is nothing to disable (see
    /// `ffi::dart_project_disable_impeller`). Under a software rasterizer an
    /// Impeller-built text display list is FATAL ("Impeller DlText cannot be
    /// drawn to a Skia canvas", spike finding L-F4), and the software
    /// renderer is the configuration this port is verified under (WSLg has
    /// no usable GL). On GL-capable hardware the embedder's own
    /// `FLUTTER_LINUX_RENDERER` variable chooses the renderer; this crate
    /// does not override it.
    ///
    /// # Errors
    ///
    /// [`BackendError::Unavailable`] if the expected files are missing from
    /// `data_dir` or any embedder constructor returns null. A `true` return
    /// means the realize ran and with it the engine start; the spike verified
    /// Dart is serving by the time realize returns (L-F2).
    pub(crate) fn boot(data_dir: &str, target: BootTarget) -> Result<Self, BackendError> {
        let data = Path::new(data_dir)
            .canonicalize()
            .unwrap_or_else(|_| Path::new(data_dir).to_path_buf());
        let assets = data.join("flutter_assets");
        let icu = data.join("icudtl.dat");
        if !assets.is_dir() {
            return Err(BackendError::Unavailable(format!(
                "flutter_assets not found at {} — is this the data directory of a \
                 `flutter build linux --debug` bundle?",
                assets.display()
            )));
        }
        if !icu.is_file() {
            return Err(BackendError::Unavailable(format!(
                "icudtl.dat not found at {}",
                icu.display()
            )));
        }
        let assets_c = CString::new(assets.to_string_lossy().into_owned())
            .map_err(|_| BackendError::Unavailable("assets path contains a NUL".to_string()))?;
        let icu_c = CString::new(icu.to_string_lossy().into_owned())
            .map_err(|_| BackendError::Unavailable("icu path contains a NUL".to_string()))?;

        // SAFETY: plain GObject construction on the main thread; the CStrings
        // are live for the setter calls (the project copies them).
        let (engine, view, messenger) = unsafe {
            let project = ffi::fl_dart_project_new();
            if project.is_null() {
                return Err(BackendError::Unavailable(
                    "fl_dart_project_new returned null".to_string(),
                ));
            }
            ffi::fl_dart_project_set_assets_path(project, assets_c.as_ptr() as *mut c_char);
            ffi::fl_dart_project_set_icu_data_path(project, icu_c.as_ptr() as *mut c_char);
            // Resolved at runtime, not linked: older engines lack the symbol
            // — and predate the Impeller-on-GTK default it opts out of, so
            // "not found" means there is nothing to disable. See
            // `ffi::dart_project_disable_impeller`.
            ffi::dart_project_disable_impeller(project);
            let view = ffi::fl_view_new(project);
            if view.is_null() {
                return Err(BackendError::Unavailable(
                    "fl_view_new returned null".to_string(),
                ));
            }
            let engine = ffi::fl_view_get_engine(view);
            if engine.is_null() {
                return Err(BackendError::Unavailable(
                    "fl_view_get_engine returned null".to_string(),
                ));
            }
            // This struct's own reference: the view's container will hold the
            // view; the engine must outlive whatever GTK does to it.
            gobject_sys::g_object_ref(engine as *mut gobject_sys::GObject);
            let messenger = ffi::fl_engine_get_binary_messenger(engine);
            if messenger.is_null() {
                gobject_sys::g_object_unref(engine as *mut gobject_sys::GObject);
                return Err(BackendError::Unavailable(
                    "fl_engine_get_binary_messenger returned null".to_string(),
                ));
            }
            (engine, view, messenger)
        };

        let (window, attachable) = match target {
            BootTarget::Window(window) => (window, true),
            BootTarget::Headless => {
                let holding = gtk::Window::new(gtk::WindowType::Toplevel);
                holding.set_default_size(500, 500);
                (holding, false)
            }
        };

        // SAFETY: `view` is a live FlView (a GtkWidget subclass);
        // from_glib_none takes its own reference, sinking the constructor's
        // floating one.
        let view_widget: gtk::Widget =
            unsafe { gtk::Widget::from_glib_none(view as *mut gtk::ffi::GtkWidget) };
        // A tao window carries a default GtkBox; a plain holding window is a
        // bare GtkBin. Either way the view fills whatever container is there.
        if let Some(container) = find_box(&window) {
            container.pack_start(&view_widget, true, true, 0);
        } else {
            window.add(&view_widget);
        }
        view_widget.show_all();
        // Realize without MAPPING — the boot itself (L-F2). The toplevel gets
        // its GDK resources unmapped, then the whole tree realizes
        // parents-first, the order GTK uses during mapping. Piecemeal calls
        // are not enough: the engine start hangs off the renderer's realize,
        // and the renderer sits under an event box that must be realized
        // before it.
        window.realize();
        realize_tree(window.upcast_ref::<gtk::Widget>());

        Ok(FlutterEngine {
            engine,
            view,
            messenger,
            window,
            attachable,
            attached: false,
            shut_down: false,
        })
    }

    /// Maps the window the view has lived in since boot, then restarts
    /// Dart's frame scheduling.
    ///
    /// After the map this sends `AppLifecycleState.inactive` then
    /// `AppLifecycleState.resumed` on `flutter/lifecycle`; the intermediate
    /// `inactive` step is not optional — the framework's transition table
    /// only allows `hidden -> inactive -> resumed`, and jumping straight to
    /// `resumed` trips a framework assertion. See [`FlutterEngine::detach`]
    /// for the matching half.
    ///
    /// # Errors
    ///
    /// [`BackendError::Unavailable`] if a view is already attached (detach
    /// first), or if this engine was booted headless — a headless engine's
    /// view lives in a private holding window, and moving it out would
    /// unrealize it, which is fatal on Linux (L-F3). The lifecycle sends are
    /// fire-and-forget GObject calls and do not fail individually.
    pub(crate) fn attach(&mut self) -> Result<(), BackendError> {
        if self.attached {
            return Err(BackendError::Unavailable(
                "a view is already attached to this engine — detach it first".to_string(),
            ));
        }
        if !self.attachable {
            return Err(BackendError::Unavailable(
                "this engine was booted headless — on Linux the view cannot move into a window \
                 after boot; open the window before start_renderer"
                    .to_string(),
            ));
        }
        self.window.show();
        self.attached = true;
        self.set_lifecycle_state("AppLifecycleState.inactive")?;
        self.set_lifecycle_state("AppLifecycleState.resumed")?;
        Ok(())
    }

    /// Stops Dart's frame scheduling, then unmaps the window.
    ///
    /// **This carries the macOS F1 fix.** Before the unmap this sends
    /// `AppLifecycleState.inactive` then `AppLifecycleState.hidden`; `hidden`
    /// is what disables Dart's frame scheduling, and the `inactive` step must
    /// come first — the framework's transition table only allows
    /// `resumed -> inactive -> hidden`. The unmap itself
    /// (`gtk_widget_hide`) never unrealizes, so the engine — and every
    /// watcher a guest armed — lives on; the spike measured frames all but
    /// stopping while hidden and resuming on show (L-F3's W2 cycle).
    ///
    /// Detaching when no view is attached is a no-op, not an error.
    ///
    /// # Errors
    ///
    /// Currently infallible (the sends are fire-and-forget GObject calls);
    /// returns `Result` to keep the three backends' shapes identical.
    pub(crate) fn detach(&mut self) -> Result<(), BackendError> {
        if !self.attached {
            return Ok(());
        }
        let lifecycle = self
            .set_lifecycle_state("AppLifecycleState.inactive")
            .and_then(|()| self.set_lifecycle_state("AppLifecycleState.hidden"));
        self.window.hide();
        self.attached = false;
        lifecycle
    }

    /// Shuts the engine down. **This is what reclaims memory** — the
    /// embedder's shutdown runs in `FlEngine`'s dispose, and this method
    /// *forces* that dispose rather than waiting for a last-reference drop
    /// that never comes (see the module docs: one embedder-internal
    /// reference outlives the view, measured on this machine).
    ///
    /// Detaches first (so the F1 sends run), destroys the view widget (which
    /// logs the embedder's harmless "The implicit view cannot be removed"
    /// warning — the implicit view is not removable, only its engine is
    /// stoppable), destroys the private holding window if this was a
    /// headless boot, then runs `g_object_run_dispose` on the engine and
    /// drops this struct's reference. A [`crate::FlutterSurface`] that still
    /// holds its references keeps the engine and messenger *objects* alive —
    /// its sends degrade to safe no-ops — but Dart is stopped and the memory
    /// reclaimed here, not at the surface's later drop.
    ///
    /// Idempotent: a second call is a no-op. Infallible for the macOS
    /// crate's reason — there is no different action `shutdown` could take
    /// with an error, since the handle is dropped immediately after either
    /// way.
    pub(crate) fn shutdown(&mut self) {
        let _ = self.detach();
        if self.shut_down {
            return;
        }
        self.shut_down = true;
        // SAFETY: `view` is the live implicit view; destroy tears the widget
        // down regardless of outstanding references, and the engine survives
        // it through this struct's own reference.
        unsafe {
            let widget: gtk::Widget =
                gtk::Widget::from_glib_none(self.view as *mut gtk::ffi::GtkWidget);
            widget.destroy();
        }
        self.view = std::ptr::null_mut();
        if !self.attachable {
            // The private holding window of a headless boot is this struct's
            // to destroy. An attachable engine's window belongs to the
            // backend's window bookkeeping instead.
            unsafe { self.window.destroy() };
        }
        // Dispose is *forced*, not awaited. Destroying the view does not
        // bring the engine's reference count to one: measured on this
        // machine, one embedder-internal reference outlives the view
        // (ref_count was 2 here after `widget.destroy()`), and an engine
        // whose dispose never runs never runs `FlutterEngineShutdown` — the
        // first lifecycle probe measured teardown reclaiming 5.4% of the
        // engine's cost instead of most of it. `g_object_run_dispose` is the
        // GObject idiom for exactly this (it is what `gtk_widget_destroy`
        // does to widgets): run the dispose machinery now, breaking the
        // internal cycle, and let the remaining holders finalize the husk
        // whenever they drop.
        //
        // SAFETY: the engine object is alive (this struct's reference), and
        // dispose on a live GObject is memory-safe by contract; this struct
        // then drops the one reference it took in `boot`.
        unsafe {
            gobject_sys::g_object_run_dispose(self.engine as *mut gobject_sys::GObject);
            gobject_sys::g_object_unref(self.engine as *mut gobject_sys::GObject);
        }
    }

    /// Sends `state` as a raw UTF-8 message on the `flutter/lifecycle`
    /// channel — a `BasicMessageChannel<String?>` with `StringCodec`, so the
    /// payload is the bytes themselves, no envelope. The same primitive
    /// [`crate::FlutterSurface`] pushes through for `duet/rpc`.
    ///
    /// # Errors
    ///
    /// Currently infallible — `fl_binary_messenger_send_on_channel` is
    /// fire-and-forget with no synchronous failure — but returns `Result` so
    /// the three backends' engine APIs stay identical.
    pub(crate) fn set_lifecycle_state(&self, state: &str) -> Result<(), BackendError> {
        let channel = CString::new("flutter/lifecycle").expect("channel name has no interior NUL");
        let payload = ffi::bytes_new(state.as_bytes());
        // SAFETY: messenger is live (the engine object is, via this struct's
        // reference); the payload reference is ours to drop after the call.
        unsafe {
            ffi::fl_binary_messenger_send_on_channel(
                self.messenger,
                channel.as_ptr(),
                payload,
                std::ptr::null_mut(),
                None,
                std::ptr::null_mut(),
            );
            glib_sys::g_bytes_unref(payload);
        }
        Ok(())
    }

    /// Borrows this engine's messenger for a [`crate::FlutterSurface`].
    ///
    /// An accessor rather than a public field, for the macOS crate's reason:
    /// a caller needs the messenger to register a channel handler, and
    /// handing out the raw `FlEngine*` would let it drive the lifecycle this
    /// type exists to sequence. The returned handle takes its own references
    /// on both the engine and the messenger objects, so a surface's sends
    /// can never dangle — even after [`FlutterEngine::shutdown`] has forced
    /// the engine's dispose, when they degrade to safe no-ops.
    ///
    /// # Errors
    ///
    /// Infallible today; `Result` keeps the three backends diff-comparable.
    pub(crate) fn binary_messenger(&self) -> Result<Messenger, BackendError> {
        // SAFETY: engine and messenger are live per this struct's reference.
        Ok(unsafe { Messenger::for_engine(self.engine, self.messenger) })
    }
}

impl Drop for FlutterEngine {
    /// Guarantees the engine reference is dropped even if a caller forgets
    /// `shutdown` on an early error path — an engine leaked past its handle
    /// would never reclaim its memory.
    fn drop(&mut self) {
        if !self.shut_down {
            self.shutdown();
        }
    }
}

/// A messenger handle holding its own references.
///
/// Holds a reference on **both** GObjects: the engine and the
/// `FlBinaryMessenger` itself. The engine reference keeps the engine object
/// alive; the messenger reference is what makes sends through a
/// [`crate::FlutterSurface`] that outlives `destroy_renderer` memory-safe —
/// [`FlutterEngine::shutdown`] forces the engine's dispose, after which the
/// engine's own messenger pointer is not this handle's to trust, but a
/// referenced `FlBinaryMessenger` outlives it and holds only a *weak*
/// engine reference internally, so a send after shutdown degrades to a
/// no-op instead of a dangling call. The documented order is still to drop
/// surfaces first.
pub struct Messenger {
    engine: ffi::FlEngineRef,
    raw: ffi::MessengerRef,
}

impl Messenger {
    /// Takes new engine and messenger references and wraps the messenger.
    ///
    /// # Safety
    ///
    /// `engine` must be a live `FlEngine*` and `raw` its messenger.
    pub(crate) unsafe fn for_engine(engine: ffi::FlEngineRef, raw: ffi::MessengerRef) -> Self {
        // SAFETY: delegated to this function's contract.
        unsafe {
            gobject_sys::g_object_ref(engine as *mut gobject_sys::GObject);
            gobject_sys::g_object_ref(raw as *mut gobject_sys::GObject);
        }
        Messenger { engine, raw }
    }

    /// The raw messenger, for registration calls.
    pub(crate) fn raw(&self) -> ffi::MessengerRef {
        self.raw
    }

    /// Sends `bytes` on `channel`, fire-and-forget.
    pub(crate) fn send(&self, channel: &CString, bytes: &[u8]) {
        let payload = ffi::bytes_new(bytes);
        // SAFETY: the messenger's engine is alive via this handle's
        // reference; the payload reference is ours to drop after the call.
        unsafe {
            ffi::fl_binary_messenger_send_on_channel(
                self.raw,
                channel.as_ptr(),
                payload,
                std::ptr::null_mut(),
                None,
                std::ptr::null_mut(),
            );
            glib_sys::g_bytes_unref(payload);
        }
    }
}

impl Drop for Messenger {
    fn drop(&mut self) {
        // SAFETY: this handle took exactly one reference on each object.
        unsafe {
            gobject_sys::g_object_unref(self.raw as *mut gobject_sys::GObject);
            gobject_sys::g_object_unref(self.engine as *mut gobject_sys::GObject);
        }
    }
}

/// The first `GtkBox` child of `window`, if any — tao windows carry one.
fn find_box(window: &gtk::Window) -> Option<gtk::Box> {
    window
        .children()
        .into_iter()
        .find_map(|child| child.downcast::<gtk::Box>().ok())
}

/// Realizes `widget` and then its children, parents-first — the order GTK
/// mapping produces, reproduced for a tree that is deliberately not mapped.
fn realize_tree(widget: &gtk::Widget) {
    widget.realize();
    if let Some(container) = widget.downcast_ref::<gtk::Container>() {
        for child in container.children() {
            realize_tree(&child);
        }
    }
}
