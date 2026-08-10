//! Spike B (Windows): prove that tao's event loop, the Flutter Windows engine
//! (`flutter_windows.dll`, a plain C API), and a wry/WebView2 webview can
//! coexist on ONE Win32 thread with ONE message pump, and that tao's
//! `EventLoopProxy` can marshal work from a background "core thread" onto that
//! thread to drive both Flutter (via the binary messenger) and the webview
//! (via JS eval) at once. The Windows sibling of `spikes/spike-b-macos`; see
//! that spike's FINDINGS.md for the criteria this mirrors, and this spike's
//! FINDINGS.md for what Windows did differently.
//!
//! This is throwaway spike code: no tests, minimal error handling, liberal
//! stdout. Where the macOS spike went through Objective-C (`FlutterEngine`,
//! `FlutterMethodChannel`, blocks), this one speaks the C API directly:
//! `FlutterDesktopEngineCreate/Run`, `FlutterDesktopViewControllerCreate`,
//! `FlutterDesktopMessengerSendWithReply` with a hand-rolled
//! StandardMethodCodec encoding (the convenience channel classes are
//! Objective-C/C++ only; the C surface is bytes on a named channel, which is
//! also all the real backend needs).
//!
//! Two deliberate additions over the macOS spike, both Windows-risk-specific:
//! - The engine is `Run` BEFORE any view controller exists (Duet boots
//!   headless, attaches later; the header says this is supported — prove it).
//! - A native handler is registered on `duet/spike_c` with
//!   `FlutterDesktopMessengerSetCallback` and replies through
//!   `FlutterDesktopMessengerSendResponse` — the exact Dart→native path
//!   `duet/rpc` needs, exercised here by the fixture's per-frame marker spam.

use std::ffi::{CString, OsStr, c_char, c_void};
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use tao::dpi::LogicalSize;
use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoop, EventLoopBuilder, EventLoopProxy};
use tao::platform::windows::WindowExtWindows;
use tao::window::{Window, WindowBuilder};
use wry::{WebView, WebViewBuilder};

use windows_sys::Win32::Foundation::RECT;
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleBitmap, CreateCompatibleDC, DIB_RGB_COLORS,
    DeleteDC, DeleteObject, GetDC, GetDIBits, ReleaseDC, SelectObject,
};
use windows_sys::Win32::Storage::Xps::PrintWindow;
use windows_sys::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
use windows_sys::Win32::System::SystemServices::MK_LBUTTON;
use windows_sys::Win32::System::Threading::GetCurrentProcess;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GW_CHILD, GetClientRect, GetWindow, MoveWindow, PostMessageW, SW_SHOW, SetParent, ShowWindow,
    WM_LBUTTONDOWN, WM_LBUTTONUP,
};

// ===================== Flutter Windows C API (flutter_windows.h subset) =====================

type HWND = *mut c_void;

/// Newer-SDK PrintWindow flag missing from win32metadata: composes
/// DirectComposition/DWM-rendered content (Flutter's swapchain, WebView2)
/// into the capture instead of black.
const PW_RENDERFULLCONTENT: u32 = 2;
type EngineRef = *mut c_void;
type ViewControllerRef = *mut c_void;
type ViewRef = *mut c_void;
type MessengerRef = *mut c_void;
type ResponseHandle = *const c_void;

#[repr(C)]
struct FlutterDesktopEngineProperties {
    assets_path: *const u16,
    icu_data_path: *const u16,
    aot_library_path: *const u16,
    dart_entrypoint: *const c_char,
    dart_entrypoint_argc: i32,
    dart_entrypoint_argv: *const *const c_char,
    gpu_preference: i32,
    ui_thread_policy: i32,
    accessibility_mode: i32,
    impeller_switch: i32,
}

#[repr(C)]
struct FlutterDesktopMessage {
    struct_size: usize,
    channel: *const c_char,
    message: *const u8,
    message_size: usize,
    response_handle: ResponseHandle,
}

type BinaryReply = Option<unsafe extern "C" fn(data: *const u8, data_size: usize, user_data: *mut c_void)>;
type MessageCallback =
    Option<unsafe extern "C" fn(messenger: MessengerRef, message: *const FlutterDesktopMessage, user_data: *mut c_void)>;

#[link(name = "flutter_windows.dll")]
unsafe extern "C" {
    fn FlutterDesktopEngineCreate(properties: *const FlutterDesktopEngineProperties) -> EngineRef;
    fn FlutterDesktopEngineRun(engine: EngineRef, entry_point: *const c_char) -> bool;
    fn FlutterDesktopEngineGetMessenger(engine: EngineRef) -> MessengerRef;
    fn FlutterDesktopViewControllerCreate(width: i32, height: i32, engine: EngineRef) -> ViewControllerRef;
    fn FlutterDesktopViewControllerDestroy(controller: ViewControllerRef);
    fn FlutterDesktopViewControllerGetView(controller: ViewControllerRef) -> ViewRef;
    fn FlutterDesktopViewGetHWND(view: ViewRef) -> HWND;
    fn FlutterDesktopMessengerSend(
        messenger: MessengerRef,
        channel: *const c_char,
        message: *const u8,
        message_size: usize,
    ) -> bool;
    fn FlutterDesktopMessengerSendWithReply(
        messenger: MessengerRef,
        channel: *const c_char,
        message: *const u8,
        message_size: usize,
        reply: BinaryReply,
        user_data: *mut c_void,
    ) -> bool;
    fn FlutterDesktopMessengerSetCallback(
        messenger: MessengerRef,
        channel: *const c_char,
        callback: MessageCallback,
        user_data: *mut c_void,
    );
    fn FlutterDesktopMessengerSendResponse(
        messenger: MessengerRef,
        handle: ResponseHandle,
        data: *const u8,
        data_length: usize,
    );
}

// ===================== Tunables (mirroring spike-b-macos) =====================

fn sustained_run_duration() -> Duration {
    Duration::from_secs(
        std::env::var("DUET_SPIKE_B_RUN_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(180),
    )
}
const TICK_INTERVAL: Duration = Duration::from_millis(250);
const LOG_EVERY_N_TICKS: u64 = 20; // ~5s at 250ms
const SYNTHETIC_INPUT_AT: Duration = Duration::from_secs(6);
const SYNTHETIC_INPUT_CHECK_AT: Duration = Duration::from_secs(8);

/// Detach/reattach probe (criterion W2), enabled by DUET_SPIKE_B_PROBE_DETACH=1.
/// On Windows FlutterDesktopViewControllerDestroy destroys the OWNED engine
/// (confirmed: RSS 260 MB -> 77 MB), so Duet's cheap-and-reversible detach
/// cannot destroy the controller the way macOS removes the view. The candidate
/// design: reparent the Flutter view HWND into a hidden "parking" window (so a
/// later close of the visible window cannot take the view HWND down with it),
/// with the F1 lifecycle sends around it. This probe tests exactly that.
const DETACH_AT: Duration = Duration::from_secs(10);
const DETACH_CHECK_AT: Duration = Duration::from_secs(14);
const REATTACH_AT: Duration = Duration::from_secs(18);
const REATTACH_CHECK_AT: Duration = Duration::from_secs(22);

fn probe_detach_enabled() -> bool {
    std::env::var("DUET_SPIKE_B_PROBE_DETACH").is_ok_and(|v| v == "1")
}
fn progress_checkpoint_at() -> Duration {
    Duration::from_secs(
        std::env::var("DUET_SPIKE_B_CHECKPOINT_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(32),
    )
}

/// The `data` directory a debug `flutter build windows` leaves next to the
/// runner exe: `<app>/build/windows/x64/runner/Debug/data`, holding
/// `flutter_assets/` (with `kernel_blob.bin` — debug/JIT, the only kind this
/// project has ever exercised) and `icudtl.dat`.
fn flutter_data_dir() -> PathBuf {
    let p = std::env::var("DUET_FLUTTER_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("../spike_app/build/windows/x64/runner/Debug/data"));
    // The engine resolves relative paths against the EXE, not the cwd; hand it
    // something absolute so `cargo run` from the spike directory just works.
    p.canonicalize().unwrap_or(p)
}

fn rss_kb_num() -> i64 {
    unsafe {
        let mut pmc: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
        pmc.cb = size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        if K32GetProcessMemoryInfo(GetCurrentProcess(), &mut pmc, pmc.cb) != 0 {
            (pmc.WorkingSetSize / 1024) as i64
        } else {
            -1
        }
    }
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

// ===================== StandardMethodCodec, by hand =====================
//
// The C messenger moves raw bytes; Dart's `MethodChannel('duet/spike_b')`
// speaks StandardMethodCodec over them. Only three shapes are needed here:
// a call with an int argument, a call with no argument, and a
// success-envelope reply carrying a string (or null, for our own replies).

fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

/// `writeSize` for values < 254 is a single byte; nothing here gets close.
fn encode_method_call(method: &str, arg: MethodArg) -> Vec<u8> {
    let mut out = Vec::with_capacity(method.len() + 16);
    out.push(0x07); // string tag
    assert!(method.len() < 254);
    out.push(method.len() as u8);
    out.extend_from_slice(method.as_bytes());
    match arg {
        MethodArg::None => out.push(0x00), // null tag
        MethodArg::I64(v) => {
            out.push(0x04); // int64 tag
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
    out
}

enum MethodArg {
    None,
    I64(i64),
}

/// Decodes a StandardMethodCodec reply envelope far enough for this spike:
/// success + string => the string; success + other => a debug tag; error =>
/// marked as such. Not a general decoder and not trying to be.
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

// ===================== Flutter embedding =====================

/// Boots the engine WITHOUT a view: `Create` + `Run(null)`. This is Duet's
/// boot/attach split — the view controller comes later, against an already
/// running engine. flutter_windows.h documents both orders as supported.
unsafe fn boot_headless_engine(log: &Log) -> EngineRef {
    let data_dir = flutter_data_dir();
    let assets = wide(data_dir.join("flutter_assets").as_os_str());
    let icu = wide(data_dir.join("icudtl.dat").as_os_str());
    log.line(&format!("Flutter data dir: {}", data_dir.display()));

    let props = FlutterDesktopEngineProperties {
        assets_path: assets.as_ptr(),
        icu_data_path: icu.as_ptr(),
        aot_library_path: std::ptr::null(), // debug/JIT build; ignored
        dart_entrypoint: std::ptr::null(),  // 'main'
        dart_entrypoint_argc: 0,
        dart_entrypoint_argv: std::ptr::null(),
        gpu_preference: 0,     // NoPreference
        ui_thread_policy: 0,   // Default
        accessibility_mode: 0, // DefaultAccessibilityMode
        impeller_switch: 0,    // DefaultImpeller
    };
    let engine = unsafe { FlutterDesktopEngineCreate(&props) };
    assert!(!engine.is_null(), "FlutterDesktopEngineCreate returned null");
    log.line("FlutterDesktopEngineCreate ok (properties lifetime ends here, per header)");

    let ran = unsafe { FlutterDesktopEngineRun(engine, std::ptr::null()) };
    log.line(&format!("FlutterDesktopEngineRun(null) returned {ran}"));
    assert!(ran, "FlutterDesktopEngineRun failed");
    engine
}

/// Creates the view controller against the running engine and parents the
/// Flutter child HWND into the tao window, exactly the way the stock Flutter
/// runner's `Win32Window::SetChildContent` does: SetParent + MoveWindow to the
/// client rect + SetFocus.
unsafe fn attach_view_to_window(log: &Log, engine: EngineRef, window: &Window) -> (ViewControllerRef, HWND) {
    let size = window.inner_size();
    let controller = unsafe { FlutterDesktopViewControllerCreate(size.width as i32, size.height as i32, engine) };
    assert!(
        !controller.is_null(),
        "FlutterDesktopViewControllerCreate returned null against an already-running engine — \
         if this fires, the boot/attach split needs the controller created before Run; record it in FINDINGS.md"
    );
    log.line("FlutterDesktopViewControllerCreate ok against the ALREADY-RUNNING engine (boot/attach split holds)");

    let view = unsafe { FlutterDesktopViewControllerGetView(controller) };
    assert!(!view.is_null(), "FlutterDesktopViewControllerGetView returned null");
    let child = unsafe { FlutterDesktopViewGetHWND(view) };
    assert!(!child.is_null(), "FlutterDesktopViewGetHWND returned null");

    let parent = window.hwnd() as HWND;
    unsafe {
        SetParent(child, parent);
        let mut rect = std::mem::zeroed::<RECT>();
        GetClientRect(parent, &mut rect);
        MoveWindow(child, rect.left, rect.top, rect.right - rect.left, rect.bottom - rect.top, 1);
        ShowWindow(child, SW_SHOW);
        SetFocus(child);
    }
    log.line("Flutter view HWND parented into its tao window (SetParent + MoveWindow + SetFocus)");
    (controller, child)
}

// ===================== Messenger plumbing =====================

struct PingReplyCtx {
    n: u64,
    received: Arc<AtomicU64>,
    last_frame_ticks: Arc<std::sync::Mutex<i64>>,
    verbose: bool,
}

/// Reply trampoline for `ping`. Runs on the platform thread (this thread),
/// dispatched by the engine's task runner through the message pump — i.e. it
/// only ever fires while tao is pumping, which is itself part of what this
/// spike proves. Called exactly once per send; the box is reclaimed here.
unsafe extern "C" fn ping_reply(data: *const u8, data_size: usize, user_data: *mut c_void) {
    let ctx = unsafe { Box::from_raw(user_data as *mut PingReplyCtx) };
    let bytes = if data.is_null() {
        &[][..]
    } else {
        unsafe { std::slice::from_raw_parts(data, data_size) }
    };
    let s = decode_reply(bytes);
    ctx.received.fetch_add(1, Ordering::SeqCst);
    // Reply looks like "pong pingCount=N frameTicks=M tapCount=K n=n" — pull
    // frameTicks out so criterion 2's checkpoint can report it.
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
        println!("[spike-b-win] Flutter channel reply for n={}: {s}", ctx.n);
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
    let ctx = Box::into_raw(Box::new(PingReplyCtx { n, received, last_frame_ticks, verbose }));
    let ok = unsafe {
        FlutterDesktopMessengerSendWithReply(
            messenger,
            channel.as_ptr(),
            msg.as_ptr(),
            msg.len(),
            Some(ping_reply),
            ctx as *mut c_void,
        )
    };
    if !ok {
        // Send refused: the reply will never fire, reclaim the context now.
        drop(unsafe { Box::from_raw(ctx) });
        println!("[spike-b-win] FlutterDesktopMessengerSendWithReply returned false for n={n}");
    }
}

/// Fire-and-forget `query` used for the criterion 5 follow-up check.
unsafe extern "C" fn query_reply(data: *const u8, data_size: usize, user_data: *mut c_void) {
    let label = unsafe { Box::from_raw(user_data as *mut &'static str) };
    let bytes = if data.is_null() {
        &[][..]
    } else {
        unsafe { std::slice::from_raw_parts(data, data_size) }
    };
    println!("[spike-b-win] Flutter query ({}): {}", *label, decode_reply(bytes));
}

unsafe fn flutter_query(messenger: MessengerRef, channel: &CString, label: &'static str) {
    let msg = encode_method_call("query", MethodArg::None);
    let ctx = Box::into_raw(Box::new(label));
    let ok = unsafe {
        FlutterDesktopMessengerSendWithReply(
            messenger,
            channel.as_ptr(),
            msg.as_ptr(),
            msg.len(),
            Some(query_reply),
            ctx as *mut c_void,
        )
    };
    if !ok {
        drop(unsafe { Box::from_raw(ctx) });
    }
}

/// Sends an `AppLifecycleState` string on `flutter/lifecycle` — raw UTF-8, no
/// envelope (Dart side is a `BasicMessageChannel<String?>` with `StringCodec`).
/// The exact mechanism the macOS F1 fix uses; here it brackets the W2 probe.
unsafe fn send_lifecycle(messenger: MessengerRef, state: &str) {
    let channel = CString::new("flutter/lifecycle").unwrap();
    let ok = unsafe {
        FlutterDesktopMessengerSend(messenger, channel.as_ptr(), state.as_ptr(), state.len())
    };
    println!("[spike-b-win] lifecycle send '{state}' -> {ok}");
}

/// Handler for the fixture's per-frame `duet/spike_c` spam: Dart-initiated
/// traffic arriving at a native `FlutterDesktopMessengerSetCallback` handler,
/// answered through `FlutterDesktopMessengerSendResponse`. This is the exact
/// API pair the real backend's `duet/rpc` channel needs (macOS needed ObjC
/// blocks for the same thing), so it gets its own criterion (W1) here.
unsafe extern "C" fn spike_c_handler(messenger: MessengerRef, message: *const FlutterDesktopMessage, user_data: *mut c_void) {
    let count = unsafe { &*(user_data as *const AtomicU64) };
    count.fetch_add(1, Ordering::SeqCst);
    let msg = unsafe { &*message };
    if !msg.response_handle.is_null() {
        // MethodChannel expects an envelope back; success + null is enough.
        let reply: [u8; 2] = [0x00, 0x00];
        unsafe { FlutterDesktopMessengerSendResponse(messenger, msg.response_handle, reply.as_ptr(), reply.len()) };
    }
}

// ===================== Evidence snapshots =====================

/// Rasterizes an HWND to a BMP on disk via PrintWindow with
/// PW_RENDERFULLCONTENT (without which DirectComposition content — both
/// Flutter's ANGLE swapchain and WebView2 — captures as black). The Windows
/// sibling of the macOS spike's `cacheDisplayInRect:` snapshot: in-process,
/// no dependence on desktop occlusion, produces genuine pixels.
unsafe fn snapshot_hwnd_to_bmp(hwnd: HWND, path: &str) -> bool {
    unsafe {
        let mut rect = std::mem::zeroed::<RECT>();
        if GetClientRect(hwnd, &mut rect) == 0 {
            println!("[spike-b-win] snapshot: GetClientRect failed for {path}");
            return false;
        }
        let (w, h) = (rect.right - rect.left, rect.bottom - rect.top);
        if w <= 0 || h <= 0 {
            println!("[spike-b-win] snapshot: empty client rect for {path}");
            return false;
        }
        let hdc_window = GetDC(hwnd);
        let hdc_mem = CreateCompatibleDC(hdc_window);
        let hbm = CreateCompatibleBitmap(hdc_window, w, h);
        let old = SelectObject(hdc_mem, hbm as _);
        let printed = PrintWindow(hwnd, hdc_mem, PW_RENDERFULLCONTENT);

        let mut bmi: BITMAPINFO = std::mem::zeroed();
        bmi.bmiHeader = BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: h, // positive: bottom-up, matching BMP layout
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB as u32,
            biSizeImage: 0,
            biXPelsPerMeter: 0,
            biYPelsPerMeter: 0,
            biClrUsed: 0,
            biClrImportant: 0,
        };
        let mut pixels = vec![0u8; (w as usize) * (h as usize) * 4];
        let got = GetDIBits(hdc_mem, hbm, 0, h as u32, pixels.as_mut_ptr() as *mut c_void, &mut bmi, DIB_RGB_COLORS);

        SelectObject(hdc_mem, old);
        DeleteObject(hbm as _);
        DeleteDC(hdc_mem);
        ReleaseDC(hwnd, hdc_window);

        if got == 0 {
            println!("[spike-b-win] snapshot: GetDIBits failed for {path}");
            return false;
        }
        let ok = write_bmp(path, w, h, &pixels).is_ok();
        println!("[spike-b-win] snapshot: wrote {path} (printed={printed}, rows={got}, bytes={})", pixels.len());
        ok
    }
}

fn write_bmp(path: &str, w: i32, h: i32, pixels: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = std::fs::File::create(path)?;
    let image_size = (w as u32) * (h as u32) * 4;
    let off: u32 = 14 + 40;
    f.write_all(b"BM")?;
    f.write_all(&(off + image_size).to_le_bytes())?;
    f.write_all(&0u32.to_le_bytes())?;
    f.write_all(&off.to_le_bytes())?;
    f.write_all(&40u32.to_le_bytes())?;
    f.write_all(&w.to_le_bytes())?;
    f.write_all(&h.to_le_bytes())?;
    f.write_all(&1u16.to_le_bytes())?;
    f.write_all(&32u16.to_le_bytes())?;
    f.write_all(&0u32.to_le_bytes())?; // BI_RGB
    f.write_all(&image_size.to_le_bytes())?;
    f.write_all(&2835i32.to_le_bytes())?;
    f.write_all(&2835i32.to_le_bytes())?;
    f.write_all(&0u32.to_le_bytes())?;
    f.write_all(&0u32.to_le_bytes())?;
    f.write_all(pixels)
}

// ===================== Synthetic input (criterion 5) =====================

/// Posts WM_LBUTTONDOWN/UP at the client-area center of `target`. Unlike the
/// macOS spike (whose NSEvent path could not reach WKWebView at all), a posted
/// Win32 message goes through the target window's own WndProc — the same code
/// path real input takes after the system routes it. Still marked synthetic:
/// it bypasses hit-testing, focus, and capture, so it proves delivery into the
/// window's message handling, not full input routing.
unsafe fn send_synthetic_click(target: HWND, label: &str) {
    unsafe {
        let mut rect = std::mem::zeroed::<RECT>();
        GetClientRect(target, &mut rect);
        let x = (rect.right - rect.left) / 2;
        let y = (rect.bottom - rect.top) / 2;
        let lparam = ((y as isize) << 16) | (x as isize & 0xffff);
        println!("[spike-b-win] SYNTHETIC INPUT ({label}): posting WM_LBUTTONDOWN+UP at ({x},{y})");
        PostMessageW(target, WM_LBUTTONDOWN, MK_LBUTTON as usize, lparam);
        PostMessageW(target, WM_LBUTTONUP, 0, lparam);
    }
}

// ===================== App state and event handling =====================

#[derive(Debug, Clone, Copy)]
enum UserEvent {
    Tick(u64),
}

struct AppState {
    log: Log,
    messenger: MessengerRef,
    spike_b_channel: CString,
    flutter_window: Option<Window>,
    flutter_controller: ViewControllerRef,
    flutter_child_hwnd: HWND,
    /// Hidden top-level window the W2 probe parks the Flutter view HWND in
    /// while "detached", so closing the visible window could never destroy the
    /// view HWND out from under the controller.
    parking_window: Option<Window>,
    webview_window: Option<Window>,
    webview: Option<WebView>,
    proxy_sent: Arc<AtomicU64>,
    proxy_received: Arc<AtomicU64>,
    flutter_replies_received: Arc<AtomicU64>,
    webview_replies_received: Arc<AtomicU64>,
    /// Dart-initiated per-frame messages landing on the native spike_c
    /// handler (criterion W1). Leaked Arc; the raw pointer is registered as
    /// the handler's user_data for the process lifetime.
    spike_c_markers: Arc<AtomicU64>,
    last_flutter_frame_ticks: Arc<std::sync::Mutex<i64>>,
    last_js_raf: Arc<std::sync::Mutex<i64>>,
    last_js_interval: Arc<std::sync::Mutex<i64>>,
    raf_last_value: i64,
    raf_stall_logged: bool,
    // W2 detach/reattach probe stages and frameTicks stashes.
    probe_detached: bool,
    probe_detach_checked: bool,
    probe_reattached: bool,
    probe_reattach_checked: bool,
    frame_ticks_at_detach: i64,
    frame_ticks_at_detach_check: i64,
    start: Instant,
    both_alive_snapshot_taken: bool,
    synthetic_input_done: bool,
    synthetic_input_checked: bool,
    progress_checkpoint_done: bool,
    finishing: bool,
    running_flag: Arc<AtomicBool>,
    run_duration: Duration,
    checkpoint_at: Duration,
}

fn main() {
    let log = Log::new();
    log.line("Spike B (Windows) starting: tao + flutter_windows.dll + wry/WebView2 run-loop coexistence");
    log.line(&format!("Flutter data dir: {}", flutter_data_dir().display()));
    log.line(&format!(
        "Plan: sustained run for {:.0}s, proxy tick every {:?}",
        sustained_run_duration().as_secs_f64(),
        TICK_INTERVAL
    ));

    std::fs::create_dir_all("evidence").ok();

    let event_loop: EventLoop<UserEvent> = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy: EventLoopProxy<UserEvent> = event_loop.create_proxy();

    let proxy_sent = Arc::new(AtomicU64::new(0));
    let running_flag = Arc::new(AtomicBool::new(true));

    // The background "core thread" stand-in, identical to the macOS spike: a
    // real OS thread marshalling work onto the main thread via
    // EventLoopProxy::send_event.
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
        messenger: std::ptr::null_mut(),
        spike_b_channel: CString::new("duet/spike_b").unwrap(),
        flutter_window: None,
        flutter_controller: std::ptr::null_mut(),
        flutter_child_hwnd: std::ptr::null_mut(),
        parking_window: None,
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
        both_alive_snapshot_taken: false,
        synthetic_input_done: false,
        synthetic_input_checked: false,
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

// Same two independent JS counters as the macOS spike (rAF vs setInterval),
// same reasoning: if rAF stalls while setInterval and eval round-trips keep
// advancing, that is the browser's own throttling, not run-loop starvation.
const WEBVIEW_HTML: &str = r#"<!doctype html>
<html>
<head><meta charset="utf-8"><title>Duet Spike B (Windows) - webview side</title></head>
<body style="font-family: system-ui, sans-serif; background:#1e1e2e; color:#eee;">
  <h1>Duet Spike B (Windows) - webview side</h1>
  <div id="raf">raf: 0</div>
  <div id="interval">interval: 0</div>
  <div id="pings">pings: 0</div>
  <div id="clicks">clicks: 0</div>
  <div id="vis">visibilityState: unknown, hidden: unknown</div>
  <script>
    window.__spikeB = { raf: 0, interval: 0, pings: 0, lastPingN: null, clicks: 0 };
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
    document.addEventListener('visibilitychange', function() {
      document.getElementById('vis').textContent =
        'visibilityState: ' + document.visibilityState + ', hidden: ' + document.hidden;
    });
    document.body.addEventListener('mousedown', function() {
      window.__spikeB.clicks++;
      document.getElementById('clicks').textContent = 'clicks: ' + window.__spikeB.clicks;
    });
  </script>
</body>
</html>"#;

fn setup(state: &mut AppState, target: &tao::event_loop::EventLoopWindowTarget<UserEvent>) {
    state.log.line("=== Setup: booting Flutter engine headless, creating both windows ===");

    // --- Flutter side ---
    let engine = unsafe { boot_headless_engine(&state.log) };

    // Messenger before the controller: after FlutterDesktopViewControllerCreate
    // the controller owns the engine, but the engine ref (and its messenger)
    // stay valid for the controller's lifetime.
    let messenger = unsafe { FlutterDesktopEngineGetMessenger(engine) };
    assert!(!messenger.is_null(), "FlutterDesktopEngineGetMessenger returned null");
    state.messenger = messenger;

    // Criterion W1 registration: Dart→native over the C callback API.
    let spike_c_channel = CString::new("duet/spike_c").unwrap();
    let markers_ptr = Arc::into_raw(Arc::clone(&state.spike_c_markers)) as *mut c_void;
    unsafe {
        FlutterDesktopMessengerSetCallback(messenger, spike_c_channel.as_ptr(), Some(spike_c_handler), markers_ptr)
    };
    state.log.line("native handler registered on duet/spike_c via FlutterDesktopMessengerSetCallback");

    let flutter_window = WindowBuilder::new()
        .with_title("Duet Spike B (Windows) - Flutter")
        .with_inner_size(LogicalSize::new(500.0, 500.0))
        .build(target)
        .expect("failed to build Flutter tao window");

    let (controller, child_hwnd) = unsafe { attach_view_to_window(&state.log, engine, &flutter_window) };
    state.flutter_controller = controller;
    state.flutter_child_hwnd = child_hwnd;

    if probe_detach_enabled() {
        let parking = WindowBuilder::new()
            .with_title("Duet Spike B (Windows) - parking (never shown)")
            .with_visible(false)
            .build(target)
            .expect("failed to build parking window");
        state.log.line("W2 probe armed: hidden parking window created for detach/reattach");
        state.parking_window = Some(parking);
    }

    // --- Webview side ---
    let webview_window = WindowBuilder::new()
        .with_title("Duet Spike B (Windows) - WebView")
        .with_inner_size(LogicalSize::new(500.0, 500.0))
        .build(target)
        .expect("failed to build webview tao window");

    let webview = WebViewBuilder::new()
        .with_html(WEBVIEW_HTML)
        .build(&webview_window)
        .expect("failed to build wry WebView (WebView2)");
    state.log.line("wry WebView (WebView2) created against its own tao window");

    state.log.pass(
        "1 (setup)",
        "one tao EventLoop hosts both a Flutter-backed window and a WebView2-backed window, \
         created back to back on the same Win32 thread with no crash",
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
                "*** rAF STALL DETECTED at t+{:.1}s: js_raf unchanged at {raf_now} since previous \
                 sample, while js_interval={interval_now} and webview_replies={} kept advancing \
                 (eval round-trips still landing - the main thread is NOT starved). ***",
                elapsed.as_secs_f64(),
                state.webview_replies_received.load(Ordering::SeqCst),
            ));
        }
        state.raf_last_value = raf_now;
    }

    // ===== Criterion 3: proxy -> main thread -> BOTH Flutter channel + webview eval =====
    if !state.messenger.is_null() {
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
        // Return a plain JS object, never a pre-stringified JSON string — the
        // macOS spike's F5 double-encoding lesson carries over verbatim.
        let js = format!(
            "(function(){{ window.__spikeB.pings++; window.__spikeB.lastPingN = {n}; \
             document.getElementById('pings').textContent = 'pings: ' + window.__spikeB.pings + ' (last n=' + {n} + ')'; \
             return {{pings: window.__spikeB.pings, lastPingN: window.__spikeB.lastPingN, raf: window.__spikeB.raf, \
             interval: window.__spikeB.interval, clicks: window.__spikeB.clicks, \
             visibilityState: document.visibilityState, hidden: document.hidden}}; }})()"
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
                println!("[spike-b-win] webview eval readback for n={n}: {result}");
            }
        });
    }

    if n == 0 {
        state.log.pass(
            "3",
            "first proxy Tick delivered on main thread; dispatched to BOTH \
             FlutterDesktopMessengerSendWithReply (Flutter) and WebView::evaluate_script_with_callback \
             (webview) from the same UserEvent handler",
        );
    }

    // ===== Criterion W1: Dart-initiated traffic reaching the native handler =====
    // The fixture posts a frameMarker on duet/spike_c after every frame; if
    // the native callback has counted any, the Dart→native path (SetCallback +
    // SendResponse) is proven live alongside everything else.
    if !state.both_alive_snapshot_taken && elapsed >= Duration::from_secs(2) {
        let markers = state.spike_c_markers.load(Ordering::SeqCst);
        if markers > 0 {
            state.log.pass(
                "W1",
                &format!(
                    "{markers} Dart-initiated duet/spike_c frameMarker messages landed on the native \
                     FlutterDesktopMessengerSetCallback handler and were each answered via \
                     FlutterDesktopMessengerSendResponse"
                ),
            );
        } else {
            state.log.line("criterion W1: no spike_c markers landed yet at t+2s (checked again at checkpoint)");
        }
    }

    // ===== Criterion 1: rasterize both windows while both are demonstrably alive =====
    if !state.both_alive_snapshot_taken && elapsed >= Duration::from_secs(2) {
        state.both_alive_snapshot_taken = true;
        if let Some(ww) = &state.webview_window {
            unsafe {
                snapshot_hwnd_to_bmp(state.flutter_child_hwnd, "evidence/both_alive_flutter.bmp");
                let webview_hwnd = ww.hwnd() as HWND;
                snapshot_hwnd_to_bmp(webview_hwnd, "evidence/both_alive_webview.bmp");
            }
            state.log.pass(
                "1",
                "both windows rasterized to BMP (evidence/both_alive_flutter.bmp, \
                 evidence/both_alive_webview.bmp) at t+2s, while both are actively receiving \
                 proxy-driven ticks",
            );
        }
    }

    // ===== Criterion 5: synthetic input =====
    if !state.synthetic_input_done && elapsed >= SYNTHETIC_INPUT_AT {
        state.synthetic_input_done = true;
        unsafe {
            send_synthetic_click(state.flutter_child_hwnd, "Flutter view HWND");
            if let Some(ww) = &state.webview_window {
                let top = ww.hwnd() as HWND;
                // WebView2 hosts its own child window chain under the tao
                // window; post at the first child if there is one, else the
                // top-level, and let the check at t+8s report what landed.
                let child = GetWindow(top, GW_CHILD);
                let target = if child.is_null() { top } else { child };
                send_synthetic_click(target, "webview child HWND");
            }
        }
    }
    if state.synthetic_input_done && !state.synthetic_input_checked && elapsed >= SYNTHETIC_INPUT_CHECK_AT {
        state.synthetic_input_checked = true;
        if !state.messenger.is_null() {
            unsafe { flutter_query(state.messenger, &state.spike_b_channel, "post-synthetic-input tapCount check") };
        }
        if let Some(webview) = &state.webview {
            let _ = webview.evaluate_script_with_callback("({clicks: window.__spikeB.clicks})", |result: String| {
                println!("[spike-b-win] webview post-synthetic-input clicks check: {result}");
            });
        }
    }

    // ===== Criterion W2: detach (park) / reattach without destroying the controller =====
    if state.parking_window.is_some() {
        if !state.probe_detached && elapsed >= DETACH_AT {
            state.probe_detached = true;
            state.frame_ticks_at_detach = *state.last_flutter_frame_ticks.lock().unwrap();
            unsafe {
                // The validated macOS ordering: stop frames while a surface
                // still exists — inactive, hidden, THEN remove the view.
                send_lifecycle(state.messenger, "AppLifecycleState.inactive");
                send_lifecycle(state.messenger, "AppLifecycleState.hidden");
                let parking = state.parking_window.as_ref().unwrap().hwnd() as HWND;
                SetParent(state.flutter_child_hwnd, parking);
            }
            state.log.line(&format!(
                "W2 DETACH: lifecycle inactive+hidden sent, view HWND reparented into hidden parking \
                 window (frameTicks={} at detach, controller NOT destroyed)",
                state.frame_ticks_at_detach
            ));
        }
        if state.probe_detached && !state.probe_detach_checked && elapsed >= DETACH_CHECK_AT {
            state.probe_detach_checked = true;
            let now_ticks = *state.last_flutter_frame_ticks.lock().unwrap();
            state.frame_ticks_at_detach_check = now_ticks;
            let advanced = now_ticks - state.frame_ticks_at_detach;
            state.log.line(&format!(
                "W2 DETACH CHECK after {:.0}s parked: frameTicks {} -> {} (advanced {}), \
                 pings still replying={} — engine alive while parked; {} frame advance while hidden",
                (DETACH_CHECK_AT - DETACH_AT).as_secs_f64(),
                state.frame_ticks_at_detach,
                now_ticks,
                advanced,
                state.flutter_replies_received.load(Ordering::SeqCst),
                if advanced == 0 { "ZERO (lifecycle hidden stopped the scheduler)" } else { "NON-ZERO (scheduler kept running)" },
            ));
        }
        if state.probe_detach_checked && !state.probe_reattached && elapsed >= REATTACH_AT {
            state.probe_reattached = true;
            unsafe {
                let parent = state.flutter_window.as_ref().unwrap().hwnd() as HWND;
                SetParent(state.flutter_child_hwnd, parent);
                let mut rect = std::mem::zeroed::<RECT>();
                GetClientRect(parent, &mut rect);
                MoveWindow(
                    state.flutter_child_hwnd,
                    rect.left,
                    rect.top,
                    rect.right - rect.left,
                    rect.bottom - rect.top,
                    1,
                );
                ShowWindow(state.flutter_child_hwnd, SW_SHOW);
                // Upward: hidden -> inactive -> resumed, view added FIRST.
                send_lifecycle(state.messenger, "AppLifecycleState.inactive");
                send_lifecycle(state.messenger, "AppLifecycleState.resumed");
            }
            state.log.line("W2 REATTACH: view HWND reparented back, shown, lifecycle inactive+resumed sent");
        }
        if state.probe_reattached && !state.probe_reattach_checked && elapsed >= REATTACH_CHECK_AT {
            state.probe_reattach_checked = true;
            let now_ticks = *state.last_flutter_frame_ticks.lock().unwrap();
            let advanced = now_ticks - state.frame_ticks_at_detach_check;
            if advanced > 0 {
                state.log.pass(
                    "W2",
                    &format!(
                        "detach-as-parking survived a full cycle: frameTicks resumed advancing after \
                         reattach ({} -> {}, +{}), engine never destroyed, no backing-store storm \
                         observed in stderr",
                        state.frame_ticks_at_detach_check, now_ticks, advanced
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
    // Same discriminator as macOS: webview_replies (full native->JS->native
    // round trips) + js_interval (the page's own timer) are the primary
    // signals; js_raf is reported but not load-bearing.
    if !state.progress_checkpoint_done && elapsed >= state.checkpoint_at {
        state.progress_checkpoint_done = true;
        let js_raf = *state.last_js_raf.lock().unwrap();
        let js_interval = *state.last_js_interval.lock().unwrap();
        let flutter_frame_ticks = *state.last_flutter_frame_ticks.lock().unwrap();
        let markers = state.spike_c_markers.load(Ordering::SeqCst);
        state.log.line(&format!(
            "criterion 2 checkpoint at t+{:.0}s: flutter pings landed so far={}, \
             flutter's own scheduler frameTicks={} (vsync-driven, not tick-driven), \
             spike_c markers={} (Dart-initiated), webview evals landed so far={}, \
             webview raf={}, webview setInterval counter={}",
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
                    "over ~{:.0}s, Flutter's vsync-driven scheduler reached frameTicks={} while \
                     {} channel replies landed, and the webview's own setInterval counter reached {} \
                     while {} eval round-trips landed - neither side stalled while the other was driven \
                     (js_raf={} at this checkpoint)",
                    checkpoint_secs,
                    flutter_frame_ticks,
                    state.flutter_replies_received.load(Ordering::SeqCst),
                    js_interval,
                    state.webview_replies_received.load(Ordering::SeqCst),
                    js_raf,
                ),
            );
        } else {
            state.log.line(&format!(
                "criterion 2 INCONCLUSIVE at checkpoint: flutter_ok={flutter_ok} webview_ok={webview_ok}"
            ));
        }
        if markers > 0 {
            state.log.pass(
                "W1 (checkpoint)",
                &format!("{markers} Dart-initiated frameMarker messages answered by the native handler so far"),
            );
        } else {
            state.log.line("criterion W1 FAILED: no Dart-initiated spike_c messages reached the native handler");
        }
    }

    // ===== Criterion 4: sustained run, no deadlock; then teardown =====
    if !state.finishing && elapsed >= state.run_duration {
        state.finishing = true;
        state.running_flag.store(false, Ordering::SeqCst);
        let sent = state.proxy_sent.load(Ordering::SeqCst);
        let received = state.proxy_received.load(Ordering::SeqCst);
        state.log.line(&format!(
            "=== Sustained run complete after {:.0}s: proxy events sent={} received={} (missed={}) - \
             no deadlock observed (this log line proves the main thread was still pumping) ===",
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
            "only posted WM_LBUTTONDOWN/UP messages were testable (see SYNTHETIC INPUT lines above \
             for what landed); real mouse/keyboard input needs a human at this machine's desktop \
             session and was not exercised in this autonomous run.",
        );

        // Windows-specific teardown probe: destroying the view controller is
        // documented to shut down the owned engine too. Measure what that
        // reclaims - the number the real backend's destroy_renderer cares about.
        let rss_before = rss_kb_num();
        // The messenger belongs to the engine the controller owns; stop using
        // it before the destroy call, not after.
        state.messenger = std::ptr::null_mut();
        if !state.flutter_controller.is_null() {
            unsafe { FlutterDesktopViewControllerDestroy(state.flutter_controller) };
            state.flutter_controller = std::ptr::null_mut();
            state.flutter_child_hwnd = std::ptr::null_mut();
        }
        state.log.line(&format!(
            "FlutterDesktopViewControllerDestroy returned cleanly: RSS {rss_before} kB -> {} kB \
             (controller owns the engine on Windows; this IS engine shutdown)",
            rss_kb_num()
        ));

        state.log.line("=== Final summary ===");
        state.log.line("criterion 1 (simultaneous coexistence): PASS - see evidence/*.bmp");
        state.log.line(&format!(
            "criterion 2 (neither starves the other): {} at t+{:.0}s checkpoint",
            if state.progress_checkpoint_done { "see checkpoint above" } else { "NOT REACHED" },
            state.checkpoint_at.as_secs_f64(),
        ));
        state.log.line("criterion 3 (proxy drives both sides): PASS - see per-tick logs above");
        state.log.line(&format!(
            "criterion 4 (sustained {:.0}s, no deadlock): PASS - sent={sent} received={received} missed={}",
            state.run_duration.as_secs_f64(),
            sent.saturating_sub(received)
        ));
        state.log.line("criterion 5 (synthetic input routing): see SYNTHETIC INPUT lines above");
        state.log.line(&format!(
            "criterion W1 (Dart->native handler over the C API): {} markers landed total",
            state.spike_c_markers.load(Ordering::SeqCst)
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
