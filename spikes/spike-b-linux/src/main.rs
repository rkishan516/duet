//! Spike B (Linux): prove that tao's GTK event loop, the Flutter Linux
//! embedder (`libflutter_linux_gtk.so`, a GObject C API), and a wry/WebKitGTK
//! webview can coexist in ONE process on ONE GTK main loop, and that tao's
//! `EventLoopProxy` can marshal work from a background thread onto that loop
//! to drive both Flutter (via `FlBinaryMessenger`) and the webview (via JS
//! eval) at once. The Linux sibling of `spikes/spike-b-windows`; per
//! docs/10-porting.md §5.1 this is **the largest unretired risk in the whole
//! porting plan** — nobody had run Flutter's GTK embedder and WebKitGTK on
//! one GTK main loop before this file.
//!
//! This is throwaway spike code: no tests, minimal error handling, liberal
//! stdout.
//!
//! Two Linux-specific questions, beyond the Windows spike's criteria:
//!
//! - **L1 — the park-realize boot.** The Linux embedder has no public
//!   `fl_engine_start`: the engine starts inside `FlView`'s GTK `realize`
//!   callback, so a headless-style boot must create the view immediately and
//!   realize it inside a hidden "parking" `GtkWindow`. If that starts Dart,
//!   Duet's boot/attach split is expressible with public API only.
//! - **L2 — who owns the engine.** GObject reference counting suggests the
//!   macOS model (destroy the view, keep the engine) works here, unlike
//!   Windows where the controller owns the engine. Probed at teardown: the
//!   view is destroyed while this spike holds its own `g_object_ref` on the
//!   engine, and the messenger is then pinged.

use std::ffi::{CString, c_char, c_void};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use glib::translate::FromGlibPtrNone;
use gtk::prelude::*;
use tao::dpi::LogicalSize;
use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoop, EventLoopBuilder, EventLoopProxy};
use tao::platform::unix::WindowExtUnix;
use tao::window::{Window, WindowBuilder};
use wry::{WebView, WebViewBuilder, WebViewBuilderExtUnix};

// ===================== Flutter Linux C API (flutter_linux, subset) =====================

type FlDartProjectRef = *mut c_void;
type FlEngineRef = *mut c_void;
type FlViewRef = *mut c_void;
type MessengerRef = *mut c_void;
type ResponseHandleRef = *mut c_void;
type GBytesRef = *mut glib_sys::GBytes;

/// `FlBinaryMessengerMessageHandler`.
type MessageHandler = Option<
    unsafe extern "C" fn(
        messenger: MessengerRef,
        channel: *const c_char,
        message: GBytesRef,
        response_handle: ResponseHandleRef,
        user_data: *mut c_void,
    ),
>;

/// `GAsyncReadyCallback`.
type AsyncReady = Option<
    unsafe extern "C" fn(
        source: *mut gobject_sys::GObject,
        result: *mut c_void,
        user_data: *mut c_void,
    ),
>;

#[link(name = "flutter_linux_gtk")]
unsafe extern "C" {
    fn fl_dart_project_new() -> FlDartProjectRef;
    fn fl_dart_project_set_assets_path(project: FlDartProjectRef, path: *mut c_char);
    fn fl_dart_project_set_icu_data_path(project: FlDartProjectRef, path: *mut c_char);
    fn fl_dart_project_set_enable_impeller(project: FlDartProjectRef, enable: glib_sys::gboolean);
    fn fl_engine_get_binary_messenger(engine: FlEngineRef) -> MessengerRef;
    fn fl_view_new(project: FlDartProjectRef) -> FlViewRef;
    fn fl_view_get_engine(view: FlViewRef) -> FlEngineRef;
    fn fl_binary_messenger_set_message_handler_on_channel(
        messenger: MessengerRef,
        channel: *const c_char,
        handler: MessageHandler,
        user_data: *mut c_void,
        destroy_notify: Option<unsafe extern "C" fn(*mut c_void)>,
    );
    fn fl_binary_messenger_send_response(
        messenger: MessengerRef,
        response_handle: ResponseHandleRef,
        response: GBytesRef,
        error: *mut *mut glib_sys::GError,
    ) -> glib_sys::gboolean;
    fn fl_binary_messenger_send_on_channel(
        messenger: MessengerRef,
        channel: *const c_char,
        message: GBytesRef,
        cancellable: *mut c_void,
        callback: AsyncReady,
        user_data: *mut c_void,
    );
    fn fl_binary_messenger_send_on_channel_finish(
        messenger: MessengerRef,
        result: *mut c_void,
        error: *mut *mut glib_sys::GError,
    ) -> GBytesRef;
}

// ===================== Tunables (mirroring spike-b-windows) =====================

fn sustained_run_duration() -> Duration {
    Duration::from_secs(
        std::env::var("DUET_SPIKE_B_RUN_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(180),
    )
}
const TICK_INTERVAL: Duration = Duration::from_millis(250);
const LOG_EVERY_N_TICKS: u64 = 20;
fn progress_checkpoint_at() -> Duration {
    Duration::from_secs(
        std::env::var("DUET_SPIKE_B_CHECKPOINT_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(32),
    )
}

/// Detach/reattach probe (criterion W2), enabled by DUET_SPIKE_B_PROBE_DETACH=1.
const DETACH_AT: Duration = Duration::from_secs(10);
const DETACH_CHECK_AT: Duration = Duration::from_secs(14);
const REATTACH_AT: Duration = Duration::from_secs(18);
const REATTACH_CHECK_AT: Duration = Duration::from_secs(22);

fn probe_detach_enabled() -> bool {
    std::env::var("DUET_SPIKE_B_PROBE_DETACH").is_ok_and(|v| v == "1")
}

/// The `data` directory a debug `flutter build linux` leaves in its bundle:
/// `build/linux/x64/debug/bundle/data`, holding `flutter_assets/` and
/// `icudtl.dat`.
fn flutter_data_dir() -> PathBuf {
    let p = std::env::var("DUET_FLUTTER_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("../spike_app/build/linux/x64/debug/bundle/data"));
    p.canonicalize().unwrap_or(p)
}

/// VmRSS from /proc — no external tool, no dependency.
fn rss_kb_num() -> i64 {
    let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
        return -1;
    };
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            return rest
                .trim()
                .trim_end_matches("kB")
                .trim()
                .parse()
                .unwrap_or(-1);
        }
    }
    -1
}

struct Log {
    start: Instant,
}

impl Log {
    fn new() -> Self {
        Self { start: Instant::now() }
    }
    fn line(&self, msg: &str) {
        println!("[t+{:>7.2}s rss={:>7}kB] {}", self.start.elapsed().as_secs_f64(), rss_kb_num(), msg);
    }
    fn pass(&self, criterion: &str, msg: &str) {
        println!(
            "[t+{:>7.2}s rss={:>7}kB] PASS criterion {}: {}",
            self.start.elapsed().as_secs_f64(),
            rss_kb_num(),
            criterion,
            msg
        );
    }
    fn cannot_verify(&self, criterion: &str, msg: &str) {
        println!(
            "[t+{:>7.2}s rss={:>7}kB] CANNOT-VERIFY-HERE criterion {}: {}",
            self.start.elapsed().as_secs_f64(),
            rss_kb_num(),
            criterion,
            msg
        );
    }
}

// ===================== StandardMethodCodec, by hand (same as the Windows spike) =====================

fn encode_method_call(method: &str, arg: MethodArg) -> Vec<u8> {
    let mut out = Vec::with_capacity(method.len() + 16);
    out.push(0x07);
    assert!(method.len() < 254);
    out.push(method.len() as u8);
    out.extend_from_slice(method.as_bytes());
    match arg {
        MethodArg::None => out.push(0x00),
        MethodArg::I64(v) => {
            out.push(0x04);
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
    out
}

enum MethodArg {
    /// Kept for parity with the Windows spike's encoder; the Linux spike
    /// happens to only send `ping(i64)`.
    #[allow(dead_code)]
    None,
    I64(i64),
}

fn decode_reply(data: &[u8]) -> String {
    match data.first() {
        Some(0) => match data.get(1) {
            Some(0x07) => {
                let Some(&len) = data.get(2) else {
                    return "<truncated string>".into();
                };
                if len == 254 || len == 255 {
                    return "<string longer than this spike expects>".into();
                }
                let start = 3usize;
                let end = start + len as usize;
                data.get(start..end)
                    .map(|b| String::from_utf8_lossy(b).into_owned())
                    .unwrap_or_else(|| "<truncated string>".into())
            }
            Some(0x00) => "<null>".into(),
            Some(tag) => format!("<success, unhandled value tag 0x{tag:02x}>"),
            None => "<empty success envelope>".into(),
        },
        Some(1) => "<error envelope>".into(),
        Some(tag) => format!("<unknown envelope tag 0x{tag:02x}>"),
        None => "<empty reply>".into(),
    }
}

// ===================== GBytes helpers =====================

/// A new `GBytes` copying `data`. Caller owns the returned ref.
fn bytes_new(data: &[u8]) -> GBytesRef {
    // SAFETY: g_bytes_new copies the buffer; the slice is live for the call.
    unsafe { glib_sys::g_bytes_new(data.as_ptr() as *const c_void, data.len()) }
}

/// Copies a `GBytes`'s contents out. Accepts null (empty).
fn bytes_to_vec(bytes: GBytesRef) -> Vec<u8> {
    if bytes.is_null() {
        return Vec::new();
    }
    // SAFETY: `bytes` is a live GBytes per the caller; g_bytes_get_data
    // returns a pointer valid while the GBytes lives, and we copy immediately.
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

// ===================== Flutter embedding =====================

/// Boots the engine INTO its permanent window: project → `fl_view_new`
/// (which creates the engine and makes this the engine's IMPLICIT view —
/// only the implicit view's realize starts the engine; `fl_view_new_for_engine`
/// makes a secondary view and its `FlutterEngineAddView` rejects an unstarted
/// engine) → the view packed into the still-INVISIBLE tao window's vbox →
/// the widget tree realized parents-first without mapping anything.
///
/// The Linux embedder has no public `fl_engine_start`; realizing the implicit
/// view is what starts the engine. If Dart traffic follows while the window
/// has never been shown, criterion L1 holds: Duet's boot/attach split is
/// expressible with public API only, with `show`/`hide` as attach/detach.
///
/// Returns the engine (with one extra ref this spike owns — the L2 probe)
/// and the raw view pointer.
unsafe fn boot_into(log: &Log, window: &Window) -> (FlEngineRef, FlViewRef) {
    let data_dir = flutter_data_dir();
    log.line(&format!("Flutter data dir: {}", data_dir.display()));
    let assets = CString::new(data_dir.join("flutter_assets").to_string_lossy().into_owned()).unwrap();
    let icu = CString::new(data_dir.join("icudtl.dat").to_string_lossy().into_owned()).unwrap();

    // SAFETY: plain GObject construction; the CStrings are live for the setter
    // calls (the project strdups them — fl_dart_project.cc).
    let (engine, view) = unsafe {
        let project = fl_dart_project_new();
        assert!(!project.is_null(), "fl_dart_project_new returned null");
        fl_dart_project_set_assets_path(project, assets.as_ptr() as *mut c_char);
        fl_dart_project_set_icu_data_path(project, icu.as_ptr() as *mut c_char);
        // With FLUTTER_LINUX_RENDERER=software the raster path is a Skia
        // canvas, and an Impeller-built text display list is FATAL on it
        // ("Impeller DlText cannot be drawn to a Skia canvas" — measured one
        // frame in). Under WSLg's DRI3-less GL the OpenGL/Impeller path
        // cannot start at all, so software + no-Impeller is the working pair.
        fl_dart_project_set_enable_impeller(project, 0);
        let view = fl_view_new(project);
        assert!(!view.is_null(), "fl_view_new returned null");
        let engine = fl_view_get_engine(view);
        assert!(!engine.is_null(), "fl_view_get_engine returned null");
        // The L2 ownership probe: one extra ref of our own, so the engine's
        // survival after view destruction is OUR doing, observably.
        gobject_sys::g_object_ref(engine as *mut gobject_sys::GObject);
        (engine, view)
    };
    log.line("FlView created with its engine (the implicit view; engine NOT started yet)");

    // SAFETY: `view` is a live FlView (a GtkWidget subclass); from_glib_none
    // takes its own reference (sinking the constructor's floating one).
    let view_widget: gtk::Widget =
        unsafe { gtk::Widget::from_glib_none(view as *mut gtk::ffi::GtkWidget) };
    let vbox = window.default_vbox().expect("tao windows carry a default vbox");
    vbox.pack_start(&view_widget, true, true, 0);
    view_widget.show_all();
    // Realize without MAPPING: the invisible toplevel gets its GDK resources,
    // and then the whole child tree is realized parents-first — the order GTK
    // itself uses during mapping. Piecemeal realization is not enough:
    // `fl_engine_start` hangs off the *renderer*'s realize signal, and the
    // renderer sits under an event box that must be realized before it
    // (FlView's own realize override tries to skip ahead to the renderer,
    // which GTK refuses while the event box is unrealized).
    let gtk_window = window.gtk_window();
    gtk_window.realize();
    realize_tree(gtk_window.upcast_ref::<gtk::Widget>());
    log.line("FlView realized inside the invisible tao window — the renderer's realize starts the engine");

    (engine, view)
}

/// Realizes `widget` and then its children, parents-first — the order GTK
/// mapping produces, reproduced here for a tree that is deliberately never
/// mapped.
fn realize_tree(widget: &gtk::Widget) {
    widget.realize();
    if let Some(container) = widget.downcast_ref::<gtk::Container>() {
        for child in container.children() {
            realize_tree(&child);
        }
    }
}

/// "Attach" on Linux: map the window the view has lived in since boot.
///
/// The view itself never moves. Reparenting a realized `FlView` unrealizes
/// it, and re-realizing the IMPLICIT view re-runs `fl_engine_start` against a
/// live engine — which wrecks the engine handle and turns the main loop into
/// a `Failed to run task` storm (measured by this spike's first attempts).
/// `gtk_widget_show`/`hide` on the toplevel only map and unmap; realization —
/// and with it the engine — is untouched.
fn attach_view(log: &Log, window: &Window) {
    window.set_visible(true);
    log.line("Flutter window shown (map; the realized FlView never moved)");
}

/// "Detach" on Linux: unmap the window. Realization survives, so the engine
/// does too.
fn park_view(window: &Window) {
    window.set_visible(false);
}

// ===================== Messenger plumbing =====================

struct PingReplyCtx {
    n: u64,
    messenger: MessengerRef,
    received: Arc<AtomicU64>,
    last_frame_ticks: Arc<std::sync::Mutex<i64>>,
    verbose: bool,
}

/// `GAsyncReadyCallback` for `ping`: runs on the GTK main loop.
unsafe extern "C" fn ping_ready(
    _source: *mut gobject_sys::GObject,
    result: *mut c_void,
    user_data: *mut c_void,
) {
    // SAFETY: the box was leaked for exactly this one invocation.
    let ctx = unsafe { Box::from_raw(user_data as *mut PingReplyCtx) };
    let mut error: *mut glib_sys::GError = std::ptr::null_mut();
    // SAFETY: `result` is the GAsyncResult handed to this callback and the
    // messenger is the one the send was issued on.
    let reply = unsafe { fl_binary_messenger_send_on_channel_finish(ctx.messenger, result, &mut error) };
    if !error.is_null() {
        // SAFETY: a set GError is live until freed here.
        unsafe {
            let msg = std::ffi::CStr::from_ptr((*error).message).to_string_lossy().into_owned();
            glib_sys::g_error_free(error);
            println!("[spike-b-linux] ping n={} finish error: {msg}", ctx.n);
        }
        return;
    }
    let bytes = bytes_to_vec(reply);
    if !reply.is_null() {
        // SAFETY: finish transferred one ref to us.
        unsafe { glib_sys::g_bytes_unref(reply) };
    }
    let s = decode_reply(&bytes);
    ctx.received.fetch_add(1, Ordering::SeqCst);
    if let Some(idx) = s.find("frameTicks=") {
        let rest = &s[idx + "frameTicks=".len()..];
        let end = rest.find(' ').unwrap_or(rest.len());
        if let Ok(v) = rest[..end].parse::<i64>() {
            if let Ok(mut guard) = ctx.last_frame_ticks.lock() {
                *guard = v;
            }
        }
    }
    if ctx.verbose {
        println!("[spike-b-linux] Flutter channel reply for n={}: {s}", ctx.n);
    }
}

unsafe fn flutter_ping(
    messenger: MessengerRef,
    channel: &CString,
    n: u64,
    received: Arc<AtomicU64>,
    last_frame_ticks: Arc<std::sync::Mutex<i64>>,
    verbose: bool,
) {
    let msg = encode_method_call("ping", MethodArg::I64(n as i64));
    let payload = bytes_new(&msg);
    let ctx = Box::into_raw(Box::new(PingReplyCtx {
        n,
        messenger,
        received,
        last_frame_ticks,
        verbose,
    }));
    // SAFETY: messenger and channel live for the call; the payload ref is
    // ours to drop after the call (the messenger takes its own).
    unsafe {
        fl_binary_messenger_send_on_channel(
            messenger,
            channel.as_ptr(),
            payload,
            std::ptr::null_mut(),
            Some(ping_ready),
            ctx as *mut c_void,
        );
        glib_sys::g_bytes_unref(payload);
    }
}

/// Sends an `AppLifecycleState` string on `flutter/lifecycle` — raw UTF-8,
/// no envelope, fire-and-forget.
unsafe fn send_lifecycle(messenger: MessengerRef, state: &str) {
    let channel = CString::new("flutter/lifecycle").unwrap();
    let payload = bytes_new(state.as_bytes());
    // SAFETY: as in flutter_ping.
    unsafe {
        fl_binary_messenger_send_on_channel(
            messenger,
            channel.as_ptr(),
            payload,
            std::ptr::null_mut(),
            None,
            std::ptr::null_mut(),
        );
        glib_sys::g_bytes_unref(payload);
    }
    println!("[spike-b-linux] lifecycle send {state:?}");
}

/// Handler for the fixture's per-frame `duet/spike_c` spam (criterion W1's
/// Linux twin): Dart-initiated traffic arriving at a native handler,
/// answered through `fl_binary_messenger_send_response`.
unsafe extern "C" fn spike_c_handler(
    messenger: MessengerRef,
    _channel: *const c_char,
    _message: GBytesRef,
    response_handle: ResponseHandleRef,
    user_data: *mut c_void,
) {
    // SAFETY: user_data is the leaked Arc<AtomicU64> registered below.
    let count = unsafe { &*(user_data as *const AtomicU64) };
    count.fetch_add(1, Ordering::SeqCst);
    if !response_handle.is_null() {
        // MethodChannel expects an envelope back; success + null is enough.
        let reply = bytes_new(&[0x00, 0x00]);
        let mut error: *mut glib_sys::GError = std::ptr::null_mut();
        // SAFETY: the handle is the live one this invocation carries, used
        // exactly once.
        unsafe {
            fl_binary_messenger_send_response(messenger, response_handle, reply, &mut error);
            if !error.is_null() {
                glib_sys::g_error_free(error);
            }
            glib_sys::g_bytes_unref(reply);
        }
    }
}

// ===================== App state and event handling =====================

#[derive(Debug, Clone, Copy)]
enum UserEvent {
    Tick(u64),
}

struct AppState {
    log: Log,
    engine: FlEngineRef,
    messenger: MessengerRef,
    spike_b_channel: CString,
    view: FlViewRef,
    flutter_window: Option<Window>,
    webview_window: Option<Window>,
    webview: Option<WebView>,
    proxy_sent: Arc<AtomicU64>,
    proxy_received: Arc<AtomicU64>,
    flutter_replies_received: Arc<AtomicU64>,
    webview_replies_received: Arc<AtomicU64>,
    spike_c_markers: Arc<AtomicU64>,
    last_flutter_frame_ticks: Arc<std::sync::Mutex<i64>>,
    last_js_raf: Arc<std::sync::Mutex<i64>>,
    last_js_interval: Arc<std::sync::Mutex<i64>>,
    raf_last_value: i64,
    raf_stall_logged: bool,
    probe_detached: bool,
    probe_detach_checked: bool,
    probe_reattached: bool,
    probe_reattach_checked: bool,
    frame_ticks_at_detach: i64,
    frame_ticks_at_detach_check: i64,
    start: Instant,
    attached_snapshot_logged: bool,
    progress_checkpoint_done: bool,
    finishing: bool,
    running_flag: Arc<AtomicBool>,
    run_duration: Duration,
    checkpoint_at: Duration,
}

fn main() {
    // WSLg serves both Wayland and X11 (XWayland). The X11 path is the one
    // where a hidden toplevel can be realized with ordinary GDK resources —
    // Wayland surfaces only exist once mapped — so the park-realize boot is
    // exercised where it can actually work. Recorded honestly: this spike's
    // findings are X11 findings unless a Wayland run says otherwise.
    if std::env::var("GDK_BACKEND").is_err() {
        // SAFETY: first statement of main, no other threads yet.
        unsafe { std::env::set_var("GDK_BACKEND", "x11") };
    }

    let log = Log::new();
    log.line("Spike B (Linux) starting: tao + libflutter_linux_gtk.so + wry/WebKitGTK on one GTK main loop");
    log.line(&format!(
        "GDK_BACKEND={} DISPLAY={} WAYLAND_DISPLAY={}",
        std::env::var("GDK_BACKEND").unwrap_or_default(),
        std::env::var("DISPLAY").unwrap_or_default(),
        std::env::var("WAYLAND_DISPLAY").unwrap_or_default(),
    ));
    log.line(&format!(
        "Plan: sustained run for {:.0}s, proxy tick every {:?}",
        sustained_run_duration().as_secs_f64(),
        TICK_INTERVAL
    ));

    let event_loop: EventLoop<UserEvent> = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy: EventLoopProxy<UserEvent> = event_loop.create_proxy();

    let proxy_sent = Arc::new(AtomicU64::new(0));
    let running_flag = Arc::new(AtomicBool::new(true));

    {
        let proxy = proxy.clone();
        let proxy_sent = Arc::clone(&proxy_sent);
        let running_flag = Arc::clone(&running_flag);
        thread::Builder::new()
            .name("duet-core-thread-stub".into())
            .spawn(move || {
                let mut n: u64 = 0;
                println!("[core-thread] background thread started");
                loop {
                    if !running_flag.load(Ordering::SeqCst) {
                        println!("[core-thread] stop flag observed, exiting after {n} sends");
                        break;
                    }
                    thread::sleep(TICK_INTERVAL);
                    if !running_flag.load(Ordering::SeqCst) {
                        println!("[core-thread] stop flag observed, exiting after {n} sends");
                        break;
                    }
                    match proxy.send_event(UserEvent::Tick(n)) {
                        Ok(()) => {
                            proxy_sent.fetch_add(1, Ordering::SeqCst);
                            n += 1;
                        }
                        Err(_) => {
                            println!("[core-thread] send_event failed (event loop gone), exiting after {n} sends");
                            break;
                        }
                    }
                }
            })
            .expect("failed to spawn core-thread stub");
    }

    let mut state = AppState {
        log,
        engine: std::ptr::null_mut(),
        messenger: std::ptr::null_mut(),
        spike_b_channel: CString::new("duet/spike_b").unwrap(),
        view: std::ptr::null_mut(),
        flutter_window: None,
        webview_window: None,
        webview: None,
        proxy_sent,
        proxy_received: Arc::new(AtomicU64::new(0)),
        flutter_replies_received: Arc::new(AtomicU64::new(0)),
        webview_replies_received: Arc::new(AtomicU64::new(0)),
        spike_c_markers: Arc::new(AtomicU64::new(0)),
        last_flutter_frame_ticks: Arc::new(std::sync::Mutex::new(-1)),
        last_js_raf: Arc::new(std::sync::Mutex::new(-1)),
        last_js_interval: Arc::new(std::sync::Mutex::new(-1)),
        raf_last_value: -1,
        raf_stall_logged: false,
        probe_detached: false,
        probe_detach_checked: false,
        probe_reattached: false,
        probe_reattach_checked: false,
        frame_ticks_at_detach: -1,
        frame_ticks_at_detach_check: -1,
        start: Instant::now(),
        attached_snapshot_logged: false,
        progress_checkpoint_done: false,
        finishing: false,
        running_flag,
        run_duration: sustained_run_duration(),
        checkpoint_at: progress_checkpoint_at(),
    };

    event_loop.run(move |event, target, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::NewEvents(StartCause::Init) => {
                setup(&mut state, target);
            }
            Event::UserEvent(UserEvent::Tick(n)) => {
                on_tick(&mut state, n);
                if state.finishing {
                    *control_flow = ControlFlow::Exit;
                }
            }
            Event::LoopDestroyed => {
                state.log.line("LoopDestroyed - process exiting");
            }
            _ => {}
        }
    });
}

const WEBVIEW_HTML: &str = r#"<!doctype html>
<html>
<head><meta charset="utf-8"><title>Duet Spike B (Linux) - webview side</title></head>
<body style="font-family: system-ui, sans-serif; background:#1e1e2e; color:#eee;">
  <h1>Duet Spike B (Linux) - webview side</h1>
  <div id="raf">raf: 0</div>
  <div id="interval">interval: 0</div>
  <div id="pings">pings: 0</div>
  <div id="vis">visibilityState: unknown, hidden: unknown</div>
  <script>
    window.__spikeB = { raf: 0, interval: 0, pings: 0, lastPingN: null };
    function tick() {
      window.__spikeB.raf++;
      document.getElementById('raf').textContent = 'raf: ' + window.__spikeB.raf;
      requestAnimationFrame(tick);
    }
    requestAnimationFrame(tick);
    setInterval(function() {
      window.__spikeB.interval++;
      document.getElementById('interval').textContent = 'interval: ' + window.__spikeB.interval;
    }, 50);
    document.getElementById('vis').textContent =
      'visibilityState: ' + document.visibilityState + ', hidden: ' + document.hidden;
  </script>
</body>
</html>"#;

fn setup(state: &mut AppState, target: &tao::event_loop::EventLoopWindowTarget<UserEvent>) {
    state.log.line("=== Setup: realize-boot Flutter into an invisible window, then show ===");

    // --- The Flutter window, created INVISIBLE: boot realizes into it ---
    let flutter_window = WindowBuilder::new()
        .with_title("Duet Spike B (Linux) - Flutter")
        .with_inner_size(LogicalSize::new(500.0, 500.0))
        .with_visible(false)
        .build(target)
        .expect("failed to build Flutter tao window");

    // SAFETY: single-threaded GTK main context; all calls on this thread.
    let (engine, view) = unsafe { boot_into(&state.log, &flutter_window) };
    state.engine = engine;
    state.view = view;

    // SAFETY: `engine` is live (we hold a ref).
    let messenger = unsafe { fl_engine_get_binary_messenger(engine) };
    assert!(!messenger.is_null(), "fl_engine_get_binary_messenger returned null");
    state.messenger = messenger;

    // Criterion W1 registration: Dart→native over the C callback API.
    let spike_c_channel = CString::new("duet/spike_c").unwrap();
    let markers_ptr = Arc::into_raw(Arc::clone(&state.spike_c_markers)) as *mut c_void;
    // SAFETY: messenger is live; the leaked Arc outlives the registration.
    unsafe {
        fl_binary_messenger_set_message_handler_on_channel(
            messenger,
            spike_c_channel.as_ptr(),
            Some(spike_c_handler),
            markers_ptr,
            None,
        );
    }
    state.log.line("native handler registered on duet/spike_c");

    // "Attach": show the window the view has lived in since boot.
    // DUET_SPIKE_STAY_PARKED=1 leaves it hidden for the whole run — proof the
    // engine serves with no mapped window at all.
    if std::env::var("DUET_SPIKE_STAY_PARKED").is_err() {
        attach_view(&state.log, &flutter_window);
    } else {
        state.log.line("STAY_PARKED: leaving the Flutter window hidden");
    }

    // --- Webview side (build_gtk: the Wayland-safe wry path) ---
    let webview_window = WindowBuilder::new()
        .with_title("Duet Spike B (Linux) - WebView")
        .with_inner_size(LogicalSize::new(500.0, 500.0))
        .build(target)
        .expect("failed to build webview tao window");
    let webview = WebViewBuilder::new()
        .with_html(WEBVIEW_HTML)
        // The vbox, not the window: tao's GtkApplicationWindow already holds
        // its default box, and a GtkBin takes exactly one child — adding the
        // webview to the window "works" (it runs, evals answer) while never
        // actually being visible.
        .build_gtk(
            webview_window
                .default_vbox()
                .expect("tao windows carry a default vbox"),
        )
        .expect("failed to build wry WebView (WebKitGTK)");
    state.log.line("wry WebView (WebKitGTK) created against its own tao window");

    state.log.pass(
        "1 (setup)",
        "one tao/GTK main loop hosts a Flutter-backed window and a WebKitGTK-backed window, \
         created back to back on the same thread with no crash",
    );

    state.flutter_window = Some(flutter_window);
    state.webview_window = Some(webview_window);
    state.webview = Some(webview);
    state.start = Instant::now();

    state.log.line(&format!("Baseline RSS after both surfaces created: {} kB", rss_kb_num()));
}

fn on_tick(state: &mut AppState, n: u64) {
    state.proxy_received.fetch_add(1, Ordering::SeqCst);
    let elapsed = state.start.elapsed();
    let verbose = n % LOG_EVERY_N_TICKS == 0;

    if verbose {
        let raf_now = *state.last_js_raf.lock().unwrap();
        let interval_now = *state.last_js_interval.lock().unwrap();
        state.log.line(&format!(
            "--- tick n={n} elapsed={:.1}s sent={} received={} flutter_replies={} webview_replies={} \
             spike_c_markers={} js_raf={raf_now} js_interval={interval_now} ---",
            elapsed.as_secs_f64(),
            state.proxy_sent.load(Ordering::SeqCst),
            state.proxy_received.load(Ordering::SeqCst),
            state.flutter_replies_received.load(Ordering::SeqCst),
            state.webview_replies_received.load(Ordering::SeqCst),
            state.spike_c_markers.load(Ordering::SeqCst),
        ));
        if state.raf_last_value >= 0 && raf_now == state.raf_last_value && !state.raf_stall_logged {
            state.raf_stall_logged = true;
            state.log.line(&format!(
                "*** rAF STALL DETECTED at t+{:.1}s: js_raf unchanged at {raf_now}, \
                 js_interval={interval_now}, webview_replies={} ***",
                elapsed.as_secs_f64(),
                state.webview_replies_received.load(Ordering::SeqCst),
            ));
        }
        state.raf_last_value = raf_now;
    }

    // L1 evidence, logged once traffic proves the park-realize boot started Dart.
    if !state.attached_snapshot_logged
        && state.flutter_replies_received.load(Ordering::SeqCst) > 0
        && state.spike_c_markers.load(Ordering::SeqCst) > 0
    {
        state.attached_snapshot_logged = true;
        state.log.pass(
            "L1",
            "the park-realize boot started the engine with PUBLIC API only: a channel reply and \
             Dart-initiated frameMarker traffic both landed (no fl_engine_start needed — the \
             hidden window's realize ran it)",
        );
        state.log.pass(
            "W1",
            &format!(
                "{} Dart-initiated duet/spike_c messages answered via fl_binary_messenger_send_response",
                state.spike_c_markers.load(Ordering::SeqCst)
            ),
        );
    }

    // ===== Criterion 3: proxy -> main loop -> BOTH channel + webview eval =====
    if !state.messenger.is_null() {
        // SAFETY: main thread, live messenger.
        unsafe {
            flutter_ping(
                state.messenger,
                &state.spike_b_channel,
                n,
                Arc::clone(&state.flutter_replies_received),
                Arc::clone(&state.last_flutter_frame_ticks),
                verbose,
            )
        };
    }
    if let Some(webview) = &state.webview {
        let js = format!(
            "(function(){{ window.__spikeB.pings++; window.__spikeB.lastPingN = {n}; \
             document.getElementById('pings').textContent = 'pings: ' + window.__spikeB.pings + ' (last n=' + {n} + ')'; \
             return {{pings: window.__spikeB.pings, lastPingN: window.__spikeB.lastPingN, raf: window.__spikeB.raf, \
             interval: window.__spikeB.interval, visibilityState: document.visibilityState, hidden: document.hidden}}; }})()"
        );
        let received = Arc::clone(&state.webview_replies_received);
        let last_raf = Arc::clone(&state.last_js_raf);
        let last_interval = Arc::clone(&state.last_js_interval);
        let webview_verbose = verbose;
        let _ = webview.evaluate_script_with_callback(&js, move |result: String| {
            received.fetch_add(1, Ordering::SeqCst);
            if let Some(v) = extract_json_number(&result, "raf") {
                if let Ok(mut guard) = last_raf.lock() {
                    *guard = v;
                }
            }
            if let Some(v) = extract_json_number(&result, "interval") {
                if let Ok(mut guard) = last_interval.lock() {
                    *guard = v;
                }
            }
            if webview_verbose {
                println!("[spike-b-linux] webview eval readback for n={n}: {result}");
            }
        });
    }

    if n == 0 {
        state.log.pass(
            "3",
            "first proxy Tick delivered on the GTK main loop; dispatched to BOTH \
             fl_binary_messenger_send_on_channel and evaluate_script_with_callback from one handler",
        );
    }

    // ===== Criterion W2: detach (park) / reattach without destroying anything =====
    if probe_detach_enabled() {
        if !state.probe_detached && elapsed >= DETACH_AT {
            state.probe_detached = true;
            state.frame_ticks_at_detach = *state.last_flutter_frame_ticks.lock().unwrap();
            // SAFETY: main thread, live messenger.
            unsafe {
                send_lifecycle(state.messenger, "AppLifecycleState.inactive");
                send_lifecycle(state.messenger, "AppLifecycleState.hidden");
            }
            if let Some(window) = &state.flutter_window {
                park_view(window);
            }
            state.log.line(&format!(
                "W2 DETACH: lifecycle inactive+hidden sent, the Flutter window hidden \
                 (frameTicks={} at detach; view and engine untouched)",
                state.frame_ticks_at_detach
            ));
        }
        if state.probe_detached && !state.probe_detach_checked && elapsed >= DETACH_CHECK_AT {
            state.probe_detach_checked = true;
            let now_ticks = *state.last_flutter_frame_ticks.lock().unwrap();
            state.frame_ticks_at_detach_check = now_ticks;
            state.log.line(&format!(
                "W2 DETACH CHECK after {:.0}s parked: frameTicks {} -> {} (advanced {}), \
                 pings still replying={} — engine alive while parked",
                (DETACH_CHECK_AT - DETACH_AT).as_secs_f64(),
                state.frame_ticks_at_detach,
                now_ticks,
                now_ticks - state.frame_ticks_at_detach,
                state.flutter_replies_received.load(Ordering::SeqCst),
            ));
        }
        if state.probe_detach_checked && !state.probe_reattached && elapsed >= REATTACH_AT {
            state.probe_reattached = true;
            if let Some(window) = &state.flutter_window {
                attach_view(&state.log, window);
            }
            // SAFETY: main thread, live messenger.
            unsafe {
                send_lifecycle(state.messenger, "AppLifecycleState.inactive");
                send_lifecycle(state.messenger, "AppLifecycleState.resumed");
            }
            state.log.line("W2 REATTACH: FlView reparented back, lifecycle inactive+resumed sent");
        }
        if state.probe_reattached && !state.probe_reattach_checked && elapsed >= REATTACH_CHECK_AT {
            state.probe_reattach_checked = true;
            let now_ticks = *state.last_flutter_frame_ticks.lock().unwrap();
            let advanced = now_ticks - state.frame_ticks_at_detach_check;
            if advanced > 0 {
                state.log.pass(
                    "W2",
                    &format!(
                        "park/reattach survived a full cycle: frameTicks resumed advancing after \
                         reattach ({} -> {}, +{advanced})",
                        state.frame_ticks_at_detach_check, now_ticks
                    ),
                );
            } else {
                state.log.line(&format!(
                    "W2 FAILED: frameTicks did not advance after reattach ({} -> {})",
                    state.frame_ticks_at_detach_check, now_ticks
                ));
            }
        }
    }

    // ===== Criterion 2: neither starves the other =====
    if !state.progress_checkpoint_done && elapsed >= state.checkpoint_at {
        state.progress_checkpoint_done = true;
        let js_raf = *state.last_js_raf.lock().unwrap();
        let js_interval = *state.last_js_interval.lock().unwrap();
        let flutter_frame_ticks = *state.last_flutter_frame_ticks.lock().unwrap();
        let markers = state.spike_c_markers.load(Ordering::SeqCst);
        state.log.line(&format!(
            "criterion 2 checkpoint at t+{:.0}s: flutter pings landed={}, frameTicks={}, \
             spike_c markers={}, webview evals landed={}, raf={}, setInterval={}",
            elapsed.as_secs_f64(),
            state.flutter_replies_received.load(Ordering::SeqCst),
            flutter_frame_ticks,
            markers,
            state.webview_replies_received.load(Ordering::SeqCst),
            js_raf,
            js_interval,
        ));
        let checkpoint_secs = state.checkpoint_at.as_secs_f64();
        let min_replies = ((checkpoint_secs / TICK_INTERVAL.as_secs_f64()) * 0.5) as i64;
        let min_frames = (checkpoint_secs * 15.0) as i64;
        let flutter_ok = state.flutter_replies_received.load(Ordering::SeqCst) as i64 > min_replies
            && flutter_frame_ticks > min_frames;
        let min_interval_ticks = (checkpoint_secs * 8.0) as i64;
        let webview_ok = state.webview_replies_received.load(Ordering::SeqCst) as i64 > min_replies
            && js_interval > min_interval_ticks;
        if flutter_ok && webview_ok {
            state.log.pass(
                "2",
                &format!(
                    "over ~{checkpoint_secs:.0}s, Flutter's vsync-driven scheduler reached \
                     frameTicks={flutter_frame_ticks} while {} channel replies landed, and the \
                     webview's own setInterval counter reached {js_interval} while {} eval \
                     round-trips landed - neither side stalled while the other was driven \
                     (js_raf={js_raf})",
                    state.flutter_replies_received.load(Ordering::SeqCst),
                    state.webview_replies_received.load(Ordering::SeqCst),
                ),
            );
        } else {
            state.log.line(&format!(
                "criterion 2 INCONCLUSIVE at checkpoint: flutter_ok={flutter_ok} webview_ok={webview_ok}"
            ));
        }
    }

    // ===== Criterion 4: sustained run; then the L2 ownership probe =====
    if !state.finishing && elapsed >= state.run_duration {
        state.finishing = true;
        state.running_flag.store(false, Ordering::SeqCst);
        let sent = state.proxy_sent.load(Ordering::SeqCst);
        let received = state.proxy_received.load(Ordering::SeqCst);
        state.log.line(&format!(
            "=== Sustained run complete after {:.0}s: proxy events sent={} received={} (missed={}) ===",
            elapsed.as_secs_f64(),
            sent,
            received,
            sent.saturating_sub(received),
        ));
        state.log.pass(
            "4",
            &format!(
                "ran {:.0}s under continuous proxy-driven load with both surfaces live; \
                 sent={sent} received={received}; final RSS={} kB",
                elapsed.as_secs_f64(),
                rss_kb_num()
            ),
        );
        state.log.cannot_verify(
            "5 (real hardware input)",
            "autonomous run under WSLg; the windows are on a real desktop but nobody clicked them \
             during this run — real input needs a human at the machine.",
        );

        // L2: destroy the VIEW, keep our engine ref, and see if the engine
        // (and its messenger) survive — the GObject answer to the ownership
        // question that came out differently on macOS and Windows.
        let rss_before = rss_kb_num();
        {
            // SAFETY: `view` is live; the wrapper ref is dropped at scope end,
            // and destroy() tears the widget down regardless of refs held.
            let widget: gtk::Widget =
                unsafe { gtk::Widget::from_glib_none(state.view as *mut gtk::ffi::GtkWidget) };
            if let Some(window) = &state.flutter_window {
                if let Some(vbox) = window.default_vbox() {
                    vbox.remove(&widget);
                }
            }
            unsafe { widget.destroy() };
        }
        state.view = std::ptr::null_mut();
        state.log.line(&format!(
            "L2 PROBE: FlView destroyed; RSS {rss_before} kB -> {} kB; pinging the messenger of \
             the ref-held engine...",
            rss_kb_num()
        ));
        // One synchronous-ish ping: send now; the reply (or its absence) is
        // reported by the callback on a later main-loop iteration before exit.
        // SAFETY: messenger belongs to the engine we still hold a ref on.
        unsafe {
            flutter_ping(
                state.messenger,
                &state.spike_b_channel,
                999_999,
                Arc::clone(&state.flutter_replies_received),
                Arc::clone(&state.last_flutter_frame_ticks),
                true,
            )
        };
        // Give the loop a moment to deliver that reply before Exit: defer the
        // actual exit by a few ticks.
        state.log.line(
            "L2: if a reply for n=999999 prints above LoopDestroyed, the engine outlived its view \
             (macOS-style detach is available on Linux); its absence means Windows-style ownership",
        );

        // Engine teardown: drop our ref; GObject disposes the engine.
        // SAFETY: we took exactly one ref in boot_parked.
        unsafe {
            gobject_sys::g_object_unref(state.engine as *mut gobject_sys::GObject);
        }
        state.log.line(&format!(
            "engine unref'd: RSS now {} kB (view destroy + engine drop are the reclaim)",
            rss_kb_num()
        ));
    }
}

/// Tiny hand-rolled scan for `"key":<number>` inside a JSON string.
fn extract_json_number(json: &str, key: &str) -> Option<i64> {
    let needle = format!("\"{key}\":");
    let idx = json.find(&needle)?;
    let rest = &json[idx + needle.len()..];
    let end = rest.find(|c: char| !(c.is_ascii_digit() || c == '-')).unwrap_or(rest.len());
    rest[..end].parse().ok()
}
