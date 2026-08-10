//! The Duet playground: the showcase's guests, with a human at the controls.
//!
//! `cargo run -p duet-showcase` walks a scripted tour and exits. This binary
//! opens the same guests over the same store and then hands the keys over —
//! and unlike the tour, it will open as many of them as asked: every *boot*
//! is an additional window, each with its own renderer and its own store
//! identity, all sharing the one store.
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
//! # The controls
//!
//! The guests' panels carry their own buttons — append a line (returns),
//! append a blank one (raises), and `+`, which bumps the one counter every
//! window shares through the `increment` command. Everything only the host
//! can do is reachable from **either** panel's Host controls section and from
//! the launching terminal (`h` lists the commands): boot another Flutter or
//! WebView window, suspend/resume/tear down the newest Flutter one, tear down
//! the newest WebView one, write as the host, sample memory, quit. The
//! panels' buttons write `control.request` into the store, where this host
//! watches and obeys — lifecycle belongs to the host, so a guest can only
//! ask, and either guest kind can ask about the other.
//!
//! Closing any guest window tears that guest down; when the last window
//! closes, the playground quits.
//!
//! # Things worth doing by hand
//!
//! - Click `+` anywhere and watch every window tick together — one write, N
//!   renderers, every watcher fed from the same push.
//! - Boot two more of each, then append a line from the oldest window: four
//!   panels redraw.
//! - Suspend the newest Flutter window, click `+` a few times, resume it —
//!   it comes back already current, because a parked engine's watchers never
//!   stopped.
//! - Tear the newest Flutter window down, keep clicking, boot a new one — it
//!   rediscovers everything from the store alone.

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

    use duet::Field;
    use duet::install;
    use duet_core::{Notification, SubscriberId};
    use duet_host::WindowBackend;
    use duet_runtime::{Runtime, StoreHandle};
    use duet_supervisor::SurfaceId;
    use tao::dpi::{LogicalPosition, LogicalSize};
    use tao::event::{Event, StartCause, WindowEvent};
    use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy, EventLoopWindowTarget};
    use tao::window::{Window, WindowBuilder, WindowId};

    use duet_showcase::commands::COMMANDS;
    use duet_showcase::state::{HostNote, initial_state};

    use crate::fields::{Fields, lines};
    use crate::rss::Sample;

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

    /// How long a webview guest gets to boot and mount before the playground
    /// stops waiting for it. Generous, because the very first launch of a
    /// fresh binary can stall on things that never recur — WebView2 creating
    /// its user-data profile, Defender scanning a just-linked exe — and this
    /// program exists to be sat in front of, not to gate CI.
    const WEB_BOOT_DEADLINE: Duration = Duration::from_secs(180);

    /// One Flutter guest window: its engine's surface id, its store identity,
    /// its registered handler, and whether its view is currently parked.
    struct FlutterGuest {
        id: SurfaceId,
        ordinal: usize,
        sub: SubscriberId,
        surface: FlutterSurface,
        parked: bool,
    }

    /// Where one webview guest is in its own little boot sequence.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum WebStage {
        /// Waiting for the bootstrap page to define `window.__duet`.
        AwaitBoot,
        /// Bundle evaluated; waiting for the panel to report itself up.
        AwaitMount,
        /// Interactive.
        Up,
    }

    /// One webview guest window. Unlike Flutter guests these never touch the
    /// backend: a webview is driven directly, exactly as in the tour.
    struct WebGuest {
        window: Window,
        surface: WebviewSurface,
        ordinal: usize,
        sub: SubscriberId,
        stage: WebStage,
        born: Instant,
        probe: Arc<Mutex<WebProbe>>,
        last_trouble: String,
    }

    /// What the host can see of a webview guest from outside the store —
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
        runtime: Option<Runtime>,
        store: StoreHandle,
        fields: Fields,
        backend: PlatformBackend,
        /// Every live Flutter guest, oldest first. "Newest" commands operate
        /// on the back.
        flutters: Vec<FlutterGuest>,
        /// Every live webview guest, oldest first.
        webs: Vec<WebGuest>,
        /// Fresh `SurfaceId`s and window ordinals; never reused, so a torn
        /// down guest's identity is never mistaken for its replacement's.
        next_ordinal: usize,
        bundle: String,
        commands: Arc<Mutex<VecDeque<String>>>,
        /// Everything printed-on-change, so the terminal narrates what the
        /// panels are doing without repeating itself every 50 ms.
        last_lines: usize,
        last_counter: String,
        /// The last `control.request` acted on — the panels' host-control
        /// buttons write distinct `verb#tag-n` values there.
        last_control: String,
        /// `control.request`, bound here rather than in the tour's `Fields`
        /// because the tour deliberately ignores the control surface.
        control: Field<String>,
        /// `counter`, likewise playground-only.
        counter: Field<i64>,
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
        // The Windows analog of the App.framework: the `data` directory a
        // debug `flutter build windows` leaves next to the runner exe.
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
        let counter = match typed.field::<i64>("counter") {
            Ok(field) => field,
            Err(e) => {
                println!("FAIL: setup — counter is not a path: {e}");
                std::process::exit(1);
            }
        };

        let commands = Arc::new(Mutex::new(VecDeque::new()));
        spawn_stdin_reader(Arc::clone(&commands));

        let mut playground = Playground {
            start: Instant::now(),
            store: runtime.handle(),
            runtime: Some(runtime),
            fields,
            backend: PlatformBackend::new(flutter_bundle),
            flutters: Vec::new(),
            webs: Vec::new(),
            next_ordinal: 0,
            bundle,
            commands,
            last_lines: 0,
            last_counter: String::new(),
            last_control: String::new(),
            control,
            counter,
            samples: Vec::new(),
            host_lines_written: 0,
        };

        event_loop.run(move |event, target, control_flow| {
            match event {
                Event::NewEvents(StartCause::Init) => {
                    if let Err(reason) = boot_web(&mut playground, target, proxy.clone()) {
                        println!("FAIL: setup — {reason}");
                        std::process::exit(1);
                    }
                    if let Err(reason) = boot_flutter(&mut playground, target) {
                        println!("FAIL: setup — {reason}");
                        std::process::exit(1);
                    }
                    print_help();
                    narrate(
                        &mut playground,
                        "playground",
                        "one of each guest is live; boot more from any panel or the terminal",
                    );
                }
                Event::NewEvents(StartCause::ResumeTimeReached { .. }) => {
                    turn(&mut playground, target, &proxy);
                }
                Event::UserEvent(DuetEvent::WebviewScript { subscriber, script }) => {
                    // Every webview gets every script; each surface checks the
                    // subscriber and drops what is not its own. With N
                    // webviews that filter is not a nicety, it is the routing.
                    for guest in &playground.webs {
                        if let Err(e) = guest.surface.deliver(subscriber, &script) {
                            println!("[playground] delivering a webview reply failed: {e}");
                        }
                    }
                }
                Event::UserEvent(DuetEvent::Notifications(batch)) => {
                    deliver_pushes(&mut playground, batch);
                }
                // Closing a guest's window tears that guest down; closing the
                // last one quits.
                Event::WindowEvent {
                    event: WindowEvent::CloseRequested,
                    window_id,
                    ..
                } => {
                    close_window_of(&mut playground, window_id);
                    if playground.flutters.is_empty() && playground.webs.is_empty() {
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

    /// One 50 ms turn: advance every booting webview, then at most one
    /// command, then the on-change narration.
    fn turn(
        playground: &mut Playground,
        target: &EventLoopWindowTarget<DuetEvent>,
        proxy: &EventLoopProxy<DuetEvent>,
    ) {
        advance_web_boots(playground);
        // At most one command per turn, terminal before panels — the
        // one-action-per-turn discipline the backends document.
        let command = playground
            .commands
            .lock()
            .ok()
            .and_then(|mut queue| queue.pop_front())
            .or_else(|| take_control_request(playground));
        if let Some(line) = command {
            handle_command(playground, target, proxy, line.trim());
        }
        report_changes(playground);
    }

    /// Walks every webview guest that is still booting one step onward.
    fn advance_web_boots(playground: &mut Playground) {
        let bundle = playground.bundle.clone();
        let elapsed = playground.start.elapsed();
        for guest in &mut playground.webs {
            let probe = match guest.probe.lock() {
                Ok(guard) => guard.clone(),
                Err(_) => WebProbe::default(),
            };
            if !probe.trouble.is_empty() && probe.trouble != guest.last_trouble {
                guest.last_trouble.clone_from(&probe.trouble);
                println!(
                    "[playground] webview #{} reported trouble: {}",
                    guest.ordinal, probe.trouble
                );
            }
            match guest.stage {
                WebStage::AwaitBoot if probe.bootstrapped => {
                    if let Err(e) = guest.surface.eval(&bundle) {
                        println!(
                            "[playground] evaluating the bundle into webview #{} failed: {e}",
                            guest.ordinal
                        );
                    }
                    guest.stage = WebStage::AwaitMount;
                }
                WebStage::AwaitMount if probe.mounted => {
                    println!(
                        "[playground] {:>7.2}s  webview #{} is up",
                        elapsed.as_secs_f64(),
                        guest.ordinal
                    );
                    guest.stage = WebStage::Up;
                }
                WebStage::AwaitBoot | WebStage::AwaitMount => {
                    if guest.born.elapsed() > WEB_BOOT_DEADLINE {
                        println!(
                            "[playground] webview #{} never came up within {WEB_BOOT_DEADLINE:?} — \
                             is the bundle built? tear it down and boot another to retry",
                            guest.ordinal
                        );
                        guest.born = Instant::now(); // keep the nag rate down
                    }
                }
                WebStage::Up => {}
            }
            if guest.stage != WebStage::Up {
                request_probe(guest);
            }
        }
    }

    /// A fresh `control.request` from a panel's host-control buttons,
    /// translated to the terminal command it aliases.
    ///
    /// The `#tag-n` suffix exists so every click from every window is a
    /// distinct store value (the minimal-patch rule notifies nobody about a
    /// no-op write); the verb is what precedes it. An unrecognised verb falls
    /// through to `handle_command`'s catch-all, which names it.
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
            "[playground] {:>7.2}s  a panel asked: {verb}",
            playground.start.elapsed().as_secs_f64()
        );
        Some(
            match verb {
                "boot_flutter" => "b",
                "suspend_flutter" => "s",
                "resume_flutter" => "r",
                "teardown_flutter" => "t",
                "boot_web" => "bw",
                "teardown_web" => "tw",
                "host_line" => "w",
                "sample" => "m",
                "quit" => "q",
                other => other,
            }
            .to_string(),
        )
    }

    fn print_help() {
        println!();
        println!("=== playground: the guests are yours; the host obeys this terminal ===");
        println!(
            "  panels   append a line (returns) · append a blank one (raises) · + (shared counter)"
        );
        println!("           every panel also carries these host controls as buttons");
        println!("  b        boot another Flutter window        bw  boot another WebView window");
        println!("  t        tear the newest Flutter down       tw  tear the newest WebView down");
        println!("  s        suspend the newest Flutter (detach its view; engine stays alive)");
        println!("  r        resume it (reattach the view — it already knows what happened)");
        println!("  w        host: append a line to document.lines");
        println!("  n <txt>  host: set document.title");
        println!("  m        sample this process's memory");
        println!("  h        this list");
        println!("  q        quit cleanly (closing the last window quits too)");
        println!();
    }

    /// Executes one terminal command.
    fn handle_command(
        playground: &mut Playground,
        target: &EventLoopWindowTarget<DuetEvent>,
        proxy: &EventLoopProxy<DuetEvent>,
        line: &str,
    ) {
        match line {
            "" => {}
            "h" | "?" | "help" => print_help(),
            "b" => match boot_flutter(playground, target) {
                Ok(()) => narrate(
                    playground,
                    "boot",
                    "a new Flutter window — a fresh engine and identity over the same store",
                ),
                Err(reason) => println!("FAIL: {reason}"),
            },
            "bw" => match boot_web(playground, target, proxy.clone()) {
                Ok(()) => narrate(
                    playground,
                    "boot",
                    "a new WebView window — a fresh renderer and identity over the same store",
                ),
                Err(reason) => println!("FAIL: {reason}"),
            },
            "s" => match playground.flutters.iter_mut().rev().find(|g| !g.parked) {
                Some(guest) => {
                    let (id, ordinal) = (guest.id, guest.ordinal);
                    match playground.backend.detach_view(id) {
                        Ok(()) => {
                            mark_parked(playground, id, true);
                            narrate_owned(
                                playground,
                                "suspend",
                                format!(
                                    "Flutter #{ordinal}'s view is detached; the engine — and its \
                                     watchers — live on"
                                ),
                            );
                        }
                        Err(e) => println!("[playground] detach failed: {e}"),
                    }
                }
                None => println!("[playground] nothing to suspend: no Flutter view is attached"),
            },
            "r" => match playground.flutters.iter().rev().find(|g| g.parked) {
                Some(guest) => {
                    let (id, ordinal) = (guest.id, guest.ordinal);
                    match playground.backend.attach_view(id) {
                        Ok(()) => {
                            mark_parked(playground, id, false);
                            narrate_owned(
                                playground,
                                "resume",
                                format!(
                                    "Flutter #{ordinal}'s view is back — showing everything that \
                                     happened while it had no pixels"
                                ),
                            );
                        }
                        Err(e) => println!("[playground] attach failed: {e}"),
                    }
                }
                None => println!("[playground] nothing to resume: no Flutter view is parked"),
            },
            "t" => match playground.flutters.len() {
                0 => println!("[playground] nothing to tear down: no Flutter guest is live"),
                n => tear_down_flutter_at(playground, n - 1),
            },
            "tw" => match playground.webs.len() {
                0 => println!("[playground] nothing to tear down: no WebView guest is live"),
                n => tear_down_web_at(playground, n - 1),
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
            "m" => sample(playground, "sample"),
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

    /// Flips one Flutter guest's parked flag.
    fn mark_parked(playground: &mut Playground, id: SurfaceId, parked: bool) {
        if let Some(guest) = playground.flutters.iter_mut().find(|g| g.id == id) {
            guest.parked = parked;
        }
    }

    /// Boots one more Flutter guest: window, engine, handler, view.
    fn boot_flutter(
        playground: &mut Playground,
        target: &EventLoopWindowTarget<DuetEvent>,
    ) -> Result<(), String> {
        playground.next_ordinal += 1;
        let ordinal = playground.next_ordinal;
        let id = SurfaceId::from_raw(ordinal as u64);
        let sub = playground.store.next_subscriber_id();
        playground
            .backend
            .open_window(
                id,
                target,
                &format!("Duet playground — Flutter guest #{ordinal}"),
            )
            .map_err(|e| format!("opening Flutter #{ordinal}'s window failed: {e}"))?;
        playground
            .backend
            .start_renderer(id)
            .map_err(|e| format!("booting Flutter #{ordinal}'s engine failed: {e}"))?;
        let engine = playground.backend.engine(id).ok_or_else(|| {
            "the backend reported a booted renderer but has no engine".to_string()
        })?;
        let surface =
            FlutterSurface::with_commands(engine, playground.store.clone(), sub, &COMMANDS)
                .map_err(|e| {
                    format!("registering Flutter #{ordinal}'s duet/rpc handler failed: {e}")
                })?;
        playground
            .backend
            .attach_view(id)
            .map_err(|e| format!("attaching Flutter #{ordinal}'s view failed: {e}"))?;
        playground.flutters.push(FlutterGuest {
            id,
            ordinal,
            sub,
            surface,
            parked: false,
        });
        Ok(())
    }

    /// Boots one more webview guest: window, surface, and its own little
    /// boot sequence (the bundle is evaluated once its page reports in).
    fn boot_web(
        playground: &mut Playground,
        target: &EventLoopWindowTarget<DuetEvent>,
        proxy: EventLoopProxy<DuetEvent>,
    ) -> Result<(), String> {
        playground.next_ordinal += 1;
        let ordinal = playground.next_ordinal;
        let sub = playground.store.next_subscriber_id();
        // Cascaded, so a second window is visibly a second window rather than
        // a pixel-perfect cover over the first.
        let offset = (playground.webs.len() as f64) * 40.0;
        let window = WindowBuilder::new()
            .with_title(format!("Duet playground — WebView guest #{ordinal}"))
            .with_inner_size(LogicalSize::new(560.0, 640.0))
            .with_position(LogicalPosition::new(660.0 + offset, 80.0 + offset))
            .build(target)
            .map_err(|e| format!("creating WebView #{ordinal}'s window failed: {e}"))?;
        let surface =
            WebviewSurface::with_commands(&window, playground.store.clone(), sub, proxy, &COMMANDS)
                .map_err(|e| format!("creating WebView #{ordinal} failed: {e}"))?;
        playground.webs.push(WebGuest {
            window,
            surface,
            ordinal,
            sub,
            stage: WebStage::AwaitBoot,
            born: Instant::now(),
            probe: Arc::new(Mutex::new(WebProbe::default())),
            last_trouble: String::new(),
        });
        Ok(())
    }

    /// Tears down the Flutter guest at `index`, in the tour's order.
    ///
    /// The `flutter.*` claims are wiped only when the *last* Flutter guest
    /// goes: with several live, that subtree is their shared voice, and
    /// erasing it would erase the survivors' evidence too.
    fn tear_down_flutter_at(playground: &mut Playground, index: usize) {
        let guest = playground.flutters.remove(index);
        match playground.store.drop_subscriber(guest.sub) {
            Ok(dropped) => println!(
                "[playground] dropped {dropped} subscription(s) for Flutter #{}",
                guest.ordinal
            ),
            Err(e) => println!("[playground] dropping its subscriptions failed: {e}"),
        }
        drop(guest.surface);
        if let Err(e) = playground.backend.destroy_renderer(guest.id) {
            println!(
                "[playground] destroying Flutter #{}'s renderer failed: {e}",
                guest.ordinal
            );
        }
        playground.backend.close_window(guest.id);
        if playground.flutters.is_empty() {
            if let Err(reason) = playground.fields.flutter.clear("torn down") {
                println!("[playground] {reason}");
            }
        }
        narrate_owned(
            playground,
            "teardown",
            format!(
                "Flutter #{} no longer exists; the store keeps the document",
                guest.ordinal
            ),
        );
    }

    /// Tears down the webview guest at `index`: identity, surface, window.
    fn tear_down_web_at(playground: &mut Playground, index: usize) {
        let guest = playground.webs.remove(index);
        match playground.store.drop_subscriber(guest.sub) {
            Ok(dropped) => println!(
                "[playground] dropped {dropped} subscription(s) for WebView #{}",
                guest.ordinal
            ),
            Err(e) => println!("[playground] dropping its subscriptions failed: {e}"),
        }
        // The surface holds the wry webview that draws into the window, so it
        // goes first, then the window.
        drop(guest.surface);
        drop(guest.window);
        if playground.webs.is_empty() {
            if let Err(reason) = playground.fields.web.clear("torn down") {
                println!("[playground] {reason}");
            }
        }
        narrate_owned(
            playground,
            "teardown",
            format!(
                "WebView #{} no longer exists; the store keeps the document",
                guest.ordinal
            ),
        );
    }

    /// Maps a window's close button to the teardown of the guest it shows.
    fn close_window_of(playground: &mut Playground, window_id: WindowId) {
        if let Some(index) = playground
            .flutters
            .iter()
            .position(|g| playground.backend.window(g.id).map(Window::id) == Some(window_id))
        {
            tear_down_flutter_at(playground, index);
            return;
        }
        if let Some(index) = playground
            .webs
            .iter()
            .position(|g| g.window.id() == window_id)
        {
            tear_down_web_at(playground, index);
        }
    }

    /// Takes and prints one memory reading.
    fn sample(playground: &mut Playground, why: &'static str) {
        let reading = Sample::take(why);
        println!(
            "[playground] {:>7.2}s  rss = {} ({} Flutter, {} WebView live)",
            playground.start.elapsed().as_secs_f64(),
            reading.rendered(),
            playground.flutters.len(),
            playground.webs.len(),
        );
        playground
            .samples
            .push((playground.start.elapsed().as_secs_f64(), reading.kb, why));
    }

    /// Shuts everything down in the load-bearing order and exits 0.
    fn quit(playground: &mut Playground) -> ! {
        println!();
        println!("=== quitting ===");
        while let Some(index) = playground.flutters.len().checked_sub(1) {
            tear_down_flutter_at(playground, index);
        }
        while let Some(index) = playground.webs.len().checked_sub(1) {
            tear_down_web_at(playground, index);
        }
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

    /// Publishes what the host just did — every panel renders this line.
    fn narrate(playground: &mut Playground, act: &str, detail: &str) {
        narrate_owned(playground, act, detail.to_string());
    }

    /// [`narrate`], for a detail that had to be formatted.
    fn narrate_owned(playground: &mut Playground, act: &str, detail: String) {
        println!(
            "[playground] {:>7.2}s  {act} — {detail}",
            playground.start.elapsed().as_secs_f64()
        );
        let note = HostNote {
            act: act.to_string(),
            detail,
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
        let counter = match playground.counter.get() {
            Ok(duet::Reading::Present(value)) => value.to_string(),
            _ => "—".to_string(),
        };
        if counter != playground.last_counter {
            playground.last_counter.clone_from(&counter);
            println!(
                "[playground] {:>7.2}s  counter = {counter}",
                playground.start.elapsed().as_secs_f64()
            );
        }
    }

    /// Hands **every** notification to **every** surface; each filters on its
    /// own subscriber — the same fan-out the tour uses, now doing real work:
    /// with N guests, the per-surface filter *is* the routing.
    fn deliver_pushes(playground: &mut Playground, batch: Vec<Notification>) {
        for note in batch {
            for guest in &playground.webs {
                if let Err(e) = guest.surface.push(&note) {
                    println!(
                        "[playground] WebView #{} refused a notification: {e}",
                        guest.ordinal
                    );
                }
            }
            for guest in &playground.flutters {
                if let Err(e) = guest.surface.push(&note) {
                    println!(
                        "[playground] Flutter #{} refused a notification: {e}",
                        guest.ordinal
                    );
                }
            }
        }
    }

    /// Asks one booting webview for a fresh readback; the callback lands on a
    /// later turn.
    fn request_probe(guest: &WebGuest) {
        let slot = Arc::clone(&guest.probe);
        let outcome = guest.surface.eval_with_callback(PROBE_JS, move |json| {
            if let Ok(mut guard) = slot.lock() {
                *guard = parse_probe(&json);
            }
        });
        if let Err(e) = outcome {
            println!("[playground] a webview readback failed: {e}");
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn main() {
    app::run();
}
