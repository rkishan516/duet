//! The Duet playground: the showcase's guests, with a human at the controls.
//!
//! `cargo run -p duet-showcase` walks a scripted tour and exits. This binary
//! opens the same two guests over the same store and then hands the keys over:
//! the guests' own panels carry their buttons (append a line, append a blank
//! one that raises), and everything only the host can do — suspending,
//! resuming, tearing down and re-booting the Flutter guest, writing as the
//! host, sampling memory — is driven by single-letter commands typed into the
//! terminal that launched it. Type `h` there for the list.
//!
//! ```console
//! $ (cd examples/showcase/flutter && flutter build windows --debug)   # or macos
//! $ (cd examples/showcase/web && npm install && npm run build)
//! $ cargo run -p duet-showcase --bin playground
//! ```
//!
//! Same environment variables as the showcase itself (`DUET_APP_FRAMEWORK_PATH`
//! on macOS, `DUET_FLUTTER_BUNDLE` on Windows, `DUET_WEB_GUEST_PATH` for the
//! web bundle).
//!
//! What to try, manually, and what each thing demonstrates:
//!
//! - Click **append a line** in either panel: the same Rust `#[command]` runs,
//!   `document.lines` grows, and *both* panels' watchers redraw — one write,
//!   two renderers.
//! - Click **append a blank line**: the command raises a typed `ComposeError`
//!   and the clicking panel renders the `raised` arm.
//! - Type **`s`**: the Flutter view detaches (on Windows it is parked in a
//!   hidden window; the engine stays alive). Now click buttons in the webview
//!   panel — then **`r`**: the Flutter view comes back already knowing
//!   everything that happened while it had no pixels, because its watchers
//!   never stopped.
//! - Type **`t`**, then click around the surviving webview panel, then **`b`**:
//!   a brand-new engine with a brand-new store identity rediscovers the whole
//!   document from the store alone — including the host wiping the old guest's
//!   published claims first, so nothing it shows is a leftover.
//! - Type **`m`** around those to watch the memory come back.

#![deny(missing_docs)]

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn main() {
    println!(
        "The Duet playground's guests need a platform backend: a FlutterEngine plus a WKWebView \
         on macOS, or flutter_windows.dll plus WebView2 on Windows.\n"
    );
}

// The tour's typed field bindings and RSS sampler, compiled into this binary
// as-is: both files are deliberately free of `crate::`-relative imports so the
// two binaries can share them without this crate growing a platform-gated
// library surface.
#[cfg(any(target_os = "macos", target_os = "windows"))]
#[path = "../tour/fields.rs"]
#[allow(dead_code)] // the tour uses all of this; the playground a subset
mod fields;
#[cfg(any(target_os = "macos", target_os = "windows"))]
#[path = "../tour/rss.rs"]
#[allow(dead_code)] // likewise
mod rss;

#[cfg(any(target_os = "macos", target_os = "windows"))]
mod app {
    use std::collections::VecDeque;
    use std::io::BufRead;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use duet::install;
    use duet_core::Notification;
    use duet_host::WindowBackend;
    use duet_runtime::{Runtime, StoreHandle};
    use duet_supervisor::SurfaceId;
    use tao::dpi::{LogicalPosition, LogicalSize};
    use tao::event::{Event, StartCause, WindowEvent};
    use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy, EventLoopWindowTarget};
    use tao::window::{Window, WindowBuilder};

    use duet_showcase::commands::COMMANDS;
    use duet_showcase::state::{HostNote, initial_state};

    use crate::fields::{Fields, lines, text};
    use crate::rss::Sample;

    use duet::Field;

    /// The platform backend, under one name — the same arrangement the tour
    /// uses, for the same reason: the two backend crates export the same API.
    #[cfg(target_os = "macos")]
    use duet_backend_macos as backend;
    #[cfg(target_os = "windows")]
    use duet_backend_windows as backend;

    #[cfg(target_os = "macos")]
    type PlatformBackend = backend::MacBackend;
    #[cfg(target_os = "windows")]
    type PlatformBackend = backend::WinBackend;

    use backend::{DuetEvent, FlutterSurface, ProxySink, WebviewSurface};

    /// Milliseconds between turns. Commands are taken one per turn, which is
    /// what keeps a pasted `s` and `r` in separate event-loop turns — the gap
    /// the macOS backend documents a detach → reattach pair needs.
    const TURN_MS: u64 = 50;

    /// How long the boot phases get before the playground gives up. Only the
    /// scripted part is on a clock; once interactive, nothing times out.
    /// Generous, because the very first launch of a fresh binary can stall on
    /// things that never recur — WebView2 creating its user-data profile,
    /// Defender scanning a just-linked exe — and this program exists to be
    /// sat in front of, not to gate CI.
    const BOOT_DEADLINE: Duration = Duration::from_secs(180);

    /// Where the Flutter guest currently is, for command guards and status.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum FlutterState {
        /// Engine live, view in its window.
        Live,
        /// Engine live, view detached (parked on Windows).
        Parked,
        /// No engine at all.
        Down,
    }

    /// The boot phases, then the open-ended part.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Phase {
        /// Wait for the `wry` bootstrap page to define `window.__duet`.
        AwaitWebBoot,
        /// Evaluate the showcase bundle into the webview.
        MountWebGuest,
        /// Wait for the bundle to report its panel is up.
        AwaitWebMounted,
        /// Open a window, boot a Flutter engine, attach its view.
        BootFlutter,
        /// Yours: commands from the terminal, buttons in the panels.
        Interactive,
    }

    /// What the host can see of the webview guest from outside the store —
    /// the tour's probe, copied because it lives in a module with
    /// crate-relative imports this binary cannot include.
    #[derive(Debug, Default, Clone)]
    struct WebProbe {
        bootstrapped: bool,
        mounted: bool,
        trouble: String,
    }

    const PROBE_JS: &str = r#"(function () {
  var s = window.__duetShowcase;
  return (window.__duet ? "1" : "0")
    + (s && s.mounted ? "1" : "0")
    + "|" + ((s && s.trouble) ? String(s.trouble) : "");
})()"#;

    fn parse_probe(json: &str) -> WebProbe {
        let body = json.trim().trim_matches('"');
        let mut chars = body.chars();
        let bootstrapped = chars.next() == Some('1');
        let mounted = chars.next() == Some('1');
        if chars.next() != Some('|') {
            return WebProbe::default();
        }
        WebProbe {
            bootstrapped,
            mounted,
            trouble: chars.collect(),
        }
    }

    /// Everything the driver owns across turns.
    struct Playground {
        start: Instant,
        phase: Phase,
        runtime: Option<Runtime>,
        store: StoreHandle,
        fields: Fields,
        backend: PlatformBackend,
        flutter_id: SurfaceId,
        flutter: Option<FlutterSurface>,
        flutter_state: FlutterState,
        flutter_sub: duet_core::SubscriberId,
        webview: Option<WebviewSurface>,
        web_window: Option<Window>,
        web_sub: duet_core::SubscriberId,
        bundle: String,
        probe: Arc<Mutex<WebProbe>>,
        commands: Arc<Mutex<VecDeque<String>>>,
        /// Everything printed-on-change, so the terminal narrates what the
        /// panels are doing without repeating itself every 50 ms.
        last_lines: usize,
        last_flutter_saw: String,
        last_web_saw: String,
        last_trouble: String,
        /// The last `control.request` acted on — the webview panel's
        /// host-control buttons write distinct `verb#n` values there.
        last_control: String,
        /// `control.request`, bound here rather than in the tour's `Fields`
        /// because the tour deliberately ignores the control surface.
        control: Field<String>,
        /// Manual RSS samples, printed again as a table on quit.
        samples: Vec<(f64, Option<i64>, &'static str)>,
        host_lines_written: usize,
    }

    /// The value of `name`, or `fallback` when it is unset.
    fn env_or(name: &str, fallback: &str) -> String {
        std::env::var(name).unwrap_or_else(|_| fallback.to_string())
    }

    /// Builds the store and the backend, spawns the stdin reader, runs the loop.
    pub fn run() -> ! {
        #[cfg(target_os = "macos")]
        let flutter_bundle = env_or(
            "DUET_APP_FRAMEWORK_PATH",
            "examples/showcase/flutter/build/macos/Build/Products/Debug/App.framework",
        );
        #[cfg(target_os = "windows")]
        let flutter_bundle = env_or(
            "DUET_FLUTTER_BUNDLE",
            "examples/showcase/flutter/build/windows/x64/runner/Debug/data",
        );
        let bundle_path = env_or(
            "DUET_WEB_GUEST_PATH",
            "examples/showcase/web/build/guest.js",
        );

        println!("[playground] Flutter bundle:    {flutter_bundle}");
        println!("[playground] webview bundle:    {bundle_path}");

        let bundle = match std::fs::read_to_string(&bundle_path) {
            Ok(text) => text,
            Err(e) => {
                println!("FAIL: setup — could not read the webview guest bundle: {e}");
                println!(
                    "      Build it with:  (cd examples/showcase/web && npm install && npm run build)"
                );
                std::process::exit(1);
            }
        };

        let event_loop = EventLoopBuilder::<DuetEvent>::with_user_event().build();
        let proxy = event_loop.create_proxy();
        let runtime = Runtime::spawn(duet::Value::Null, ProxySink::new(event_loop.create_proxy()));

        let typed = match install(runtime.handle(), &initial_state()) {
            Ok(typed) => typed,
            Err(e) => {
                println!("FAIL: setup — installing the showcase state failed: {e}");
                std::process::exit(1);
            }
        };
        let fields = match Fields::bind(&typed) {
            Ok(fields) => fields,
            Err(e) => {
                println!("FAIL: setup — a field path is not a path: {e}");
                std::process::exit(1);
            }
        };
        let control = match typed.field::<String>("control.request") {
            Ok(field) => field,
            Err(e) => {
                println!("FAIL: setup — control.request is not a path: {e}");
                std::process::exit(1);
            }
        };

        let web_sub = runtime.next_subscriber_id();
        let flutter_sub = runtime.next_subscriber_id();

        let commands = Arc::new(Mutex::new(VecDeque::new()));
        spawn_stdin_reader(Arc::clone(&commands));

        let mut playground = Playground {
            start: Instant::now(),
            phase: Phase::AwaitWebBoot,
            store: runtime.handle(),
            runtime: Some(runtime),
            fields,
            backend: PlatformBackend::new(flutter_bundle),
            flutter_id: SurfaceId::from_raw(1),
            flutter: None,
            flutter_state: FlutterState::Down,
            flutter_sub,
            webview: None,
            web_window: None,
            web_sub,
            bundle,
            probe: Arc::new(Mutex::new(WebProbe::default())),
            commands,
            last_lines: 0,
            last_flutter_saw: String::new(),
            last_web_saw: String::new(),
            last_trouble: String::new(),
            last_control: String::new(),
            control,
            samples: Vec::new(),
            host_lines_written: 0,
        };

        event_loop.run(move |event, target, control_flow| {
            match event {
                Event::NewEvents(StartCause::Init) => {
                    if let Err(reason) = open_webview(&mut playground, target, proxy.clone()) {
                        println!("FAIL: setup — {reason}");
                        std::process::exit(1);
                    }
                }
                Event::NewEvents(StartCause::ResumeTimeReached { .. }) => {
                    turn(&mut playground, target);
                }
                Event::UserEvent(DuetEvent::WebviewScript { subscriber, script }) => {
                    if let Some(surface) = playground.webview.as_ref() {
                        if let Err(e) = surface.deliver(subscriber, &script) {
                            println!("[playground] delivering a webview reply failed: {e}");
                        }
                    }
                }
                Event::UserEvent(DuetEvent::Notifications(batch)) => {
                    deliver_pushes(&mut playground, batch);
                }
                // Clicking a window's close button is a command too: the
                // Flutter window's maps to teardown, the webview's to quit.
                Event::WindowEvent {
                    event: WindowEvent::CloseRequested,
                    window_id,
                    ..
                } => {
                    let flutter_window = playground
                        .backend
                        .window(playground.flutter_id)
                        .map(Window::id);
                    if Some(window_id) == flutter_window {
                        tear_down(&mut playground);
                    } else {
                        quit(&mut playground);
                    }
                }
                _ => {}
            }
            *control_flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(TURN_MS));
        });
    }

    /// Forwards terminal lines to the event loop, one queue entry per line.
    fn spawn_stdin_reader(queue: Arc<Mutex<VecDeque<String>>>) {
        std::thread::Builder::new()
            .name("playground-stdin".to_string())
            .spawn(move || {
                let stdin = std::io::stdin();
                for line in stdin.lock().lines() {
                    let Ok(line) = line else { return };
                    if let Ok(mut queue) = queue.lock() {
                        queue.push_back(line);
                    }
                }
            })
            .expect("the stdin reader should spawn");
    }

    /// Creates the webview guest's window and surface.
    fn open_webview(
        playground: &mut Playground,
        target: &EventLoopWindowTarget<DuetEvent>,
        proxy: EventLoopProxy<DuetEvent>,
    ) -> Result<(), String> {
        let window = WindowBuilder::new()
            .with_title("Duet playground — WebView guest")
            .with_inner_size(LogicalSize::new(560.0, 640.0))
            .with_position(LogicalPosition::new(660.0, 80.0))
            .build(target)
            .map_err(|e| format!("creating the webview's window failed: {e}"))?;
        let surface = WebviewSurface::with_commands(
            &window,
            playground.store.clone(),
            playground.web_sub,
            proxy,
            &COMMANDS,
        )
        .map_err(|e| format!("creating the wry webview failed: {e}"))?;
        playground.web_window = Some(window);
        playground.webview = Some(surface);
        Ok(())
    }

    /// One 50 ms turn: at most one command, then the on-change narration.
    fn turn(playground: &mut Playground, target: &EventLoopWindowTarget<DuetEvent>) {
        match playground.phase {
            Phase::AwaitWebBoot => {
                if boot_timed_out(
                    playground,
                    "the wry bootstrap page never defined window.__duet",
                ) {
                    return;
                }
                if read_probe(playground).bootstrapped {
                    playground.phase = Phase::MountWebGuest;
                }
                request_probe(playground);
            }
            Phase::MountWebGuest => {
                if let Some(surface) = playground.webview.as_ref() {
                    if let Err(e) = surface.eval(&playground.bundle) {
                        println!("[playground] evaluating the webview bundle failed: {e}");
                    }
                }
                playground.phase = Phase::AwaitWebMounted;
                request_probe(playground);
            }
            Phase::AwaitWebMounted => {
                if boot_timed_out(
                    playground,
                    "the showcase bundle never reported its panel up",
                ) {
                    return;
                }
                let probe = read_probe(playground);
                if !probe.trouble.is_empty() && probe.trouble != playground.last_trouble {
                    playground.last_trouble = probe.trouble.clone();
                    println!(
                        "[playground] the webview guest reported trouble: {}",
                        probe.trouble
                    );
                }
                if probe.mounted {
                    playground.phase = Phase::BootFlutter;
                }
                request_probe(playground);
            }
            Phase::BootFlutter => match boot_flutter(playground, target) {
                Ok(()) => {
                    print_help();
                    narrate(
                        playground,
                        "playground",
                        "both guests are live; the terminal has the host's controls",
                    );
                    playground.phase = Phase::Interactive;
                }
                Err(reason) => {
                    println!("FAIL: {reason}");
                    std::process::exit(1);
                }
            },
            Phase::Interactive => {
                // At most one command per turn, terminal before panel — the
                // one-action-per-turn discipline the backends document.
                let command = playground
                    .commands
                    .lock()
                    .ok()
                    .and_then(|mut queue| queue.pop_front())
                    .or_else(|| take_control_request(playground));
                if let Some(line) = command {
                    handle_command(playground, target, line.trim());
                }
                report_changes(playground);
            }
        }
    }

    /// A fresh `control.request` from the webview panel's host-control
    /// buttons, translated to the terminal command it aliases.
    ///
    /// The `#n` suffix exists so every click is a distinct store value (the
    /// minimal-patch rule notifies nobody about a no-op write); the verb is
    /// what precedes it. An unrecognised verb falls through to
    /// `handle_command`'s catch-all, which names it.
    fn take_control_request(playground: &mut Playground) -> Option<String> {
        let request = match playground.control.get() {
            Ok(duet::Reading::Present(value)) => value,
            _ => return None,
        };
        if request.is_empty() || request == playground.last_control {
            return None;
        }
        playground.last_control.clone_from(&request);
        let verb = request.split('#').next().unwrap_or("");
        println!(
            "[playground] {:>7.2}s  the webview panel asked: {verb}",
            playground.start.elapsed().as_secs_f64()
        );
        Some(
            match verb {
                "suspend" => "s",
                "resume" => "r",
                "teardown" => "t",
                "boot" => "b",
                "host_line" => "w",
                "sample" => "m",
                other => other,
            }
            .to_string(),
        )
    }

    /// Whether the scripted boot has been going too long; exits if so.
    fn boot_timed_out(playground: &Playground, what: &str) -> bool {
        if playground.start.elapsed() <= BOOT_DEADLINE {
            return false;
        }
        println!("FAIL: setup — {what} within {BOOT_DEADLINE:?}");
        std::process::exit(1);
    }

    fn print_help() {
        println!();
        println!("=== playground: the guests are yours; the host obeys this terminal ===");
        println!(
            "  panels   click their buttons — append a line (returns), append a blank one (raises)"
        );
        println!("  s        suspend the Flutter guest: detach its view, keep the engine");
        println!(
            "  r        resume it: reattach the view — it already knows what happened meanwhile"
        );
        println!(
            "  t        tear it down: subscriptions, surface, engine, window (state survives)"
        );
        println!("  b        boot it again: fresh engine, fresh identity, same store");
        println!("  w        host: append a line to document.lines");
        println!("  n <txt>  host: set document.title");
        println!("  m        sample this process's memory");
        println!("  h        this list");
        println!("  q        quit cleanly (closing the webview window quits too)");
        println!();
    }

    /// Executes one terminal command.
    fn handle_command(
        playground: &mut Playground,
        target: &EventLoopWindowTarget<DuetEvent>,
        line: &str,
    ) {
        match line {
            "" => {}
            "h" | "?" | "help" => print_help(),
            "s" => match playground.flutter_state {
                FlutterState::Live => match playground.backend.detach_view(playground.flutter_id) {
                    Ok(()) => {
                        playground.flutter_state = FlutterState::Parked;
                        narrate(
                            playground,
                            "suspend",
                            "the Flutter view is detached; the engine — and its watchers — live on",
                        );
                    }
                    Err(e) => println!("[playground] detach failed: {e}"),
                },
                other => {
                    println!("[playground] nothing to suspend: the Flutter guest is {other:?}")
                }
            },
            "r" => match playground.flutter_state {
                FlutterState::Parked => {
                    match playground.backend.attach_view(playground.flutter_id) {
                        Ok(()) => {
                            playground.flutter_state = FlutterState::Live;
                            narrate(
                                playground,
                                "resume",
                                "the view is back — showing everything that happened while it had no pixels",
                            );
                        }
                        Err(e) => println!("[playground] attach failed: {e}"),
                    }
                }
                other => println!("[playground] nothing to resume: the Flutter guest is {other:?}"),
            },
            "t" => tear_down(playground),
            "b" => match playground.flutter_state {
                FlutterState::Down => match boot_flutter(playground, target) {
                    Ok(()) => narrate(
                        playground,
                        "boot",
                        "a new engine, a new store identity — rediscovering the state from the store alone",
                    ),
                    Err(reason) => println!("FAIL: {reason}"),
                },
                other => {
                    println!("[playground] already booted: the Flutter guest is {other:?}")
                }
            },
            "w" => {
                playground.host_lines_written += 1;
                let mut document = lines(&playground.fields.lines);
                document.push(format!(
                    "Host: line {} , typed into the terminal.",
                    playground.host_lines_written
                ));
                match playground.fields.lines.set(&document) {
                    Ok(()) => narrate(
                        playground,
                        "host write",
                        "appended a line to document.lines",
                    ),
                    Err(e) => println!("[playground] writing document.lines failed: {e}"),
                }
            }
            "m" => sample(playground, "typed m"),
            "q" => quit(playground),
            other => {
                if let Some(title) = other.strip_prefix("n ") {
                    match playground.fields.title.set(&title.trim().to_string()) {
                        Ok(()) => narrate(playground, "host write", "set document.title"),
                        Err(e) => println!("[playground] writing document.title failed: {e}"),
                    }
                } else {
                    println!("[playground] unrecognised command {other:?} — h for the list");
                }
            }
        }
    }

    /// The whole Flutter boot: window, engine, handler, view.
    fn boot_flutter(
        playground: &mut Playground,
        target: &EventLoopWindowTarget<DuetEvent>,
    ) -> Result<(), String> {
        playground.flutter_sub = playground.store.next_subscriber_id();
        playground
            .backend
            .open_window(
                playground.flutter_id,
                target,
                "Duet playground — Flutter guest",
            )
            .map_err(|e| format!("opening the Flutter guest's window failed: {e}"))?;
        playground
            .backend
            .start_renderer(playground.flutter_id)
            .map_err(|e| format!("booting a Flutter engine failed: {e}"))?;
        let engine = playground
            .backend
            .engine(playground.flutter_id)
            .ok_or_else(|| {
                "the backend reported a booted renderer but has no engine".to_string()
            })?;
        let surface = FlutterSurface::with_commands(
            engine,
            playground.store.clone(),
            playground.flutter_sub,
            &COMMANDS,
        )
        .map_err(|e| format!("registering the duet/rpc handler failed: {e}"))?;
        playground
            .backend
            .attach_view(playground.flutter_id)
            .map_err(|e| format!("attaching the Flutter view failed: {e}"))?;
        playground.flutter = Some(surface);
        playground.flutter_state = FlutterState::Live;
        Ok(())
    }

    /// Tears the Flutter guest down, in the tour's order, and wipes its claims.
    fn tear_down(playground: &mut Playground) {
        if playground.flutter_state == FlutterState::Down {
            println!("[playground] nothing to tear down: the Flutter guest is already gone");
            return;
        }
        match playground.store.drop_subscriber(playground.flutter_sub) {
            Ok(dropped) => {
                println!("[playground] dropped {dropped} subscription(s) for the Flutter guest")
            }
            Err(e) => println!("[playground] dropping its subscriptions failed: {e}"),
        }
        drop(playground.flutter.take());
        if let Err(e) = playground.backend.destroy_renderer(playground.flutter_id) {
            println!("[playground] destroying the Flutter renderer failed: {e}");
        }
        playground.backend.close_window(playground.flutter_id);
        playground.flutter_state = FlutterState::Down;
        if let Err(reason) = playground.fields.flutter.clear("torn down") {
            println!("[playground] {reason}");
        }
        narrate(
            playground,
            "teardown",
            "the Flutter guest no longer exists; its published claims are wiped — the store keeps the document",
        );
    }

    /// Takes and prints one memory reading.
    fn sample(playground: &mut Playground, why: &'static str) {
        let reading = Sample::take(why);
        println!(
            "[playground] {:>7.2}s  rss = {} ({why})",
            playground.start.elapsed().as_secs_f64(),
            reading.rendered()
        );
        playground
            .samples
            .push((playground.start.elapsed().as_secs_f64(), reading.kb, why));
    }

    /// Shuts everything down in the load-bearing order and exits 0.
    fn quit(playground: &mut Playground) -> ! {
        println!();
        println!("=== quitting ===");
        if playground.flutter_state != FlutterState::Down {
            tear_down(playground);
        }
        playground.webview = None;
        playground.web_window = None;
        match playground.runtime.take() {
            Some(runtime) => match runtime.shutdown() {
                Ok(()) => println!("[playground] the core thread stopped cleanly"),
                Err(e) => println!("[playground] the core thread refused to stop: {e}"),
            },
            None => println!("[playground] the runtime was already gone"),
        }
        if !playground.samples.is_empty() {
            println!();
            println!("=== memory samples ===");
            for (at, kb, why) in &playground.samples {
                let rendered = kb.map_or("unreadable".to_string(), |kb| format!("{kb} kB"));
                println!("  {at:>7.2}s  {rendered:>10}  ({why})");
            }
        }
        std::process::exit(0);
    }

    /// Publishes what the host just did — both panels render this line.
    fn narrate(playground: &mut Playground, act: &str, detail: &str) {
        println!(
            "[playground] {:>7.2}s  {act} — {detail}",
            playground.start.elapsed().as_secs_f64()
        );
        let note = HostNote {
            act: act.to_string(),
            detail: detail.to_string(),
        };
        if let Err(e) = playground.fields.host.set(&note) {
            println!("[playground] publishing the host's note failed: {e}");
        }
    }

    /// Prints what changed since the last turn, so the terminal narrates the
    /// panels' activity without a human having to split attention.
    fn report_changes(playground: &mut Playground) {
        let document = lines(&playground.fields.lines);
        if document.len() != playground.last_lines {
            playground.last_lines = document.len();
            println!(
                "[playground] {:>7.2}s  document.lines now holds {} line(s); last: {:?}",
                playground.start.elapsed().as_secs_f64(),
                document.len(),
                document.last().map(String::as_str).unwrap_or("")
            );
        }
        let flutter_saw = text(&playground.fields.flutter.saw_peer_note);
        if flutter_saw != playground.last_flutter_saw {
            playground.last_flutter_saw = flutter_saw.clone();
            println!(
                "[playground] {:>7.2}s  the Flutter guest's watcher saw web.note = {flutter_saw:?}",
                playground.start.elapsed().as_secs_f64()
            );
        }
        let web_saw = text(&playground.fields.web.saw_peer_note);
        if web_saw != playground.last_web_saw {
            playground.last_web_saw = web_saw.clone();
            println!(
                "[playground] {:>7.2}s  the webview guest's watcher saw flutter.note = {web_saw:?}",
                playground.start.elapsed().as_secs_f64()
            );
        }
    }

    /// Hands **every** notification to **both** surfaces; each filters on its
    /// own subscriber — the same fan-out the tour uses, for the same reason.
    fn deliver_pushes(playground: &mut Playground, batch: Vec<Notification>) {
        for note in batch {
            if let Some(surface) = playground.webview.as_ref() {
                if let Err(e) = surface.push(&note) {
                    println!("[playground] the webview surface refused a notification: {e}");
                }
            }
            if let Some(surface) = playground.flutter.as_ref() {
                if let Err(e) = surface.push(&note) {
                    println!("[playground] the Flutter surface refused a notification: {e}");
                }
            }
        }
    }

    /// Copies the latest readback out from under its lock.
    fn read_probe(playground: &Playground) -> WebProbe {
        match playground.probe.lock() {
            Ok(guard) => guard.clone(),
            Err(_) => WebProbe::default(),
        }
    }

    /// Asks the webview guest for a fresh readback; the callback lands later.
    fn request_probe(playground: &Playground) {
        let Some(surface) = playground.webview.as_ref() else {
            return;
        };
        let slot = Arc::clone(&playground.probe);
        let outcome = surface.eval_with_callback(PROBE_JS, move |json| {
            if let Ok(mut guard) = slot.lock() {
                *guard = parse_probe(&json);
            }
        });
        if let Err(e) = outcome {
            println!("[playground] the webview readback failed: {e}");
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn main() {
    app::run();
}
