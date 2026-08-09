# The Duet showcase

One Rust host owns the state. A Flutter engine and a `wry` webview attach to it
as guests. They read and write the same store, see each other's writes, invoke
the host's commands — and halfway through, one of them is torn down, its memory
comes back, and it is booted again to find the state it never saw written.

> *State survives teardown. Events don't.*

Everything below is generated from, or checked against, a single Rust
definition: `host/src/state.rs` and `host/src/commands.rs`.

```
  host/src/state.rs  +  host/src/commands.rs        one definition
            │
            ├── cargo run --bin schema ──▶ schema/showcase.json      the contract
            │                                        │
            │                                  duet generate
            │                                   │            │
            │                    flutter/lib/src/          web/src/
            │                    showcase.duet.dart        showcase.duet.ts
            │
            └── duet::install ──▶ the live store, shared by both guests
```

## Run it

Three build steps, then one command. Each guest is a real application and has to
be built like one.

```console
$ (cd examples/showcase/flutter && flutter build macos --debug)
$ (cd examples/showcase/web && npm install && npm run build)
$ cargo run -p duet-showcase
```

Only the hand-written half of the Flutter app is tracked — `lib/`, `pubspec.yaml`
and `pubspec_overrides.yaml` — exactly as for `fixtures/duet_guest`. The Xcode
scaffolding is regenerable, so on a fresh clone run this once first:

```console
$ flutter create --platforms=macos --org com.example \
    --project-name duet_showcase examples/showcase/flutter
```

macOS only: the guests are a `FlutterEngine` and a `WKWebView`. On any other
platform the binary says so and exits; the definition half of the crate still
builds, and `cargo run -p duet-showcase --bin schema` still works.

| Variable | Default |
|---|---|
| `DUET_APP_FRAMEWORK_PATH` | `examples/showcase/flutter/build/macos/Build/Products/Debug/App.framework` |
| `DUET_WEB_GUEST_PATH` | `examples/showcase/web/build/guest.js` |
| `DUET_SHOWCASE_LINGER_SECS` | `0` — set it to pause mid-tour with both guests live, for hot reload |
| `FLUTTER_MACOS_FRAMEWORK_DIR` | the engine directory `crates/duet-backend-macos/build.rs` defaults to |

The host exits `0` if every claim held and non-zero otherwise, so it is usable
as a check and not only as a demo.

## What to look for

The host narrates. Two windows open — a Flutter one and a webview one, each with
a live panel — and the terminal prints what the host did and what each guest
reported back through the store. The run ends with a table of claims and a table
of resident set size.

### Act 1 — two guests, one store

Both guests attach, arm their watchers, write a greeting for the other to read,
and only then set `status = "ready"`. The host waits for that one field, because
each guest writes it last.

### Act 2 — each sees the other's write

The Flutter guest writes `flutter.note`; the webview guest watches it. The
webview guest writes `web.note`; the Flutter guest watches it. Each mirrors what
its watcher delivered into `saw_peer_note`, and the host checks the mirror
against **what the peer actually wrote** — not against a constant this repository
would then have in three languages.

### Act 3 — commands, both arms

Both guests call the same Rust function:

```rust
#[command]
pub fn append_line(ctx: &CommandContext, text: String) -> Result<i64, ComposeError>
```

Once with a line (`returned`), once with whitespace (`raised`, carrying a typed
`ComposeError`). Because `append_line` is the only writer of `document.lines`,
two guests append concurrently and neither clobbers the other — which is what the
document holding one line from each proves.

A **raise** is the command running and reporting a domain outcome. A host
*refusal* — no such command, an argument that would not decode — is a different
thing: it arrives as `failed` and throws out of the guest's client. The demo
distinguishes them in both guests' code.

### Act 4 — teardown, and the memory comes back

The Flutter guest's subscriptions are dropped, its surface is dropped, its engine
is destroyed and its window closed. Then RSS is sampled again. Measured on the
machine this was written on:

```
  before either guest exists             33136 kB
  webview guest live                     85120 kB
  both guests live                      240624 kB
  Flutter guest torn down               113184 kB
  Flutter guest booted again            242048 kB
```

Booting the Flutter guest cost ~155 MB; tearing it down gave ~127 MB of it back —
81 %, against a floor of 30 %. The floor is a *share* rather than a kilobyte
count for the reason `crates/duet-backend-macos/examples/lifecycle.rs` sets out:
an absolute floor silently encodes which guest app was booted.

### Act 5 — the store outlives the guest

With the Flutter guest gone, the host appends a line. The webview guest's typed
watcher fires. Nothing about the surviving guest changed.

### Act 6 — boot it again, state intact

A second engine boots for the same surface, with a fresh subscriber id. Before
the reboot the host **wipes everything the old guest published** — a torn-down
guest cannot retract its own claims, and leaving them in place would make this
act unfalsifiable. The new guest then rediscovers, from the store alone:

- every line, including the one written while it did not exist,
- the webview guest's greeting, which it never saw written.

## Hot reload

`flutter.note` is published from the Flutter guest's `build`, so editing one
constant propagates all the way through to the other renderer without anything
restarting.

```console
$ DUET_SHOWCASE_LINGER_SECS=60 cargo run -p duet-cli -- dev \
    --flutter examples/showcase/flutter \
    --flutter-root "$FLUTTER_ROOT" \
    -- cargo run -p duet-showcase
```

Wait for `hot reload is armed`, then edit `kFlutterNote` in
`flutter/lib/src/showcase_app.dart` and save. The tour pauses there — deliberately
*before* the teardown act, because `duet dev` finds the Dart VM service by
reading the first announcement out of the host's stdout, and this host boots a
second engine later with a second VM service. The only engine `duet dev` can
reload is the first one.

The Rust host is never restarted, so the store keeps its contents; `reloadSources`
patches the isolate in place, so the Dart heap keeps its `State` objects. What
the terminal prints is the constant arriving at the *other* guest's watcher.
Measured here, editing one string in one file:

```
[duet dev] 1 file(s) changed, reloading…
[duet dev] reloaded in 137 ms (recompile 37 ms, reload 53 ms, reassemble 45 ms) — 4 librar(y/ies) reloaded, 772 kept

[showcase]   8.02s  flutter.note is now "edited by hot reload, no restart"
[showcase]   8.02s  the webview guest's watcher has seen "edited by hot reload, no restart"
```

`duet dev`'s web half (Vite HMR) does not exist yet, so the webview guest is
rebuilt with `npm run build` and the host restarted.

## Where things are

| Path | What it is |
|---|---|
| `host/src/state.rs` | `#[derive(SharedState)]` — the whole shared store, in one place |
| `host/src/commands.rs` | `#[command]` — the two functions both guests invoke |
| `host/src/bin/schema.rs` | writes `schema/showcase.json` |
| `host/src/main.rs` | the entry point and the platform gate |
| `host/src/tour/` | the scripted tour: acts, typed fields, guests, RSS, report |
| `host/build.rs` | the rpath an embedder has to re-emit (see below) |
| `schema/showcase.json` | the contract; committed, and staleness-checked by a test |
| `flutter/lib/src/guest.dart` | the Flutter guest's whole Duet conversation |
| `flutter/lib/src/showcase_app.dart` | its widget tree, and the hot-reload knob |
| `flutter/lib/src/showcase.duet.dart` | generated; do not edit |
| `web/src/guest.ts` | the webview guest's whole Duet conversation |
| `web/src/panel.ts` | its DOM |
| `web/src/showcase.duet.ts` | generated; do not edit |

### Regenerating the contract and the clients

```console
$ cargo run -p duet-showcase --bin schema > examples/showcase/schema/showcase.json
$ cargo run -p duet-cli -- generate \
    --schema examples/showcase/schema/showcase.json \
    --dart examples/showcase/flutter/lib/src/showcase.duet.dart \
    --ts examples/showcase/web/src/showcase.duet.ts
```

`--check` on the same command exits `3` if either committed client is stale.
`duet-showcase`'s own test suite fails if `schema/showcase.json` stops being what
the Rust definition renders, so the chain cannot rot silently from either end.

## What could not be verified here

This is the honest part.

- **Nothing visual.** There is no reachable on-screen WindowServer for a spawned
  process on this machine. Both windows are created and both renderers draw into
  them, but no display shows either one and no human has seen this app. Every
  claim in the report is a value read back out of the store, never a pixel.
- **No mouse or keyboard.** The panels have buttons and they are wired up, but
  real input into a `WKWebView` is unproven here (a Spike B finding). Nothing the
  demo checks depends on a click: both guests run their opening moves on their
  own.
- **Debug/JIT only.** Only a debug `App.framework` has ever been built in this
  environment. Nothing here says anything about release/AOT Dart, and `duet dev`
  cannot work against one at all — a release engine has no VM service.
- **One machine, one run shape.** The RSS numbers above are from this machine.
  The *claim* is a share of measured cost, which is portable; the kilobytes are
  not.

## What the library could not do

Three things the showcase wanted and had to work around. None of them were fixed
by widening a library API, because a demo is the wrong place to do that.

1. **A webview surface cannot be given a page.** `WebviewSurface::new` and
   `with_commands` build their `wry` webview with
   `duet_webview::bootstrap::BOOTSTRAP_HTML` and expose no URL, no HTML, no
   custom protocol and no initialization script. So the webview guest is bundled
   to one file and `eval`'d into the fixed page, where the first thing it does is
   replace the body with the DOM it wishes it had been loaded into. This works —
   `duet-protocol`'s `WryTransport` is built for exactly this handoff and
   replaces `window.__duet` wholesale — but it is not how an application would
   ship a page.

2. **An embedder has to re-emit the Flutter rpath.**
   `crates/duet-backend-macos/build.rs` emits
   `-Wl,-rpath,<FlutterMacOS.framework's directory>`, and a `cargo:rustc-link-arg`
   does not reach a downstream crate's binary. A real application that depends on
   `duet-backend-macos` therefore links fine and dies at startup with
   `Library not loaded: @rpath/FlutterMacOS.framework/…`, which is what this
   showcase did on its first run. `host/build.rs` repeats the flag. The library
   could propagate it by declaring `links = "FlutterMacOS"` and emitting the
   directory as build metadata.

3. **Codegen emits no Rust accessors.** `duet generate` produces Dart and
   TypeScript. The host uses `TypedStore::field::<T>("document.lines")`, which is
   typed and validates its path once — but the path is still a hand-written
   literal, so the one place in the showcase that can go stale silently is the
   Rust side, the side that owns the definition. `host/src/tour/fields.rs` puts
   every one of them in a single struct built at startup to keep the blast radius
   small.

One more, smaller: a typed `watch` callback fires only for changes *after* the
subscription, and the value already at the path arrives separately as
`DuetWatch.current`. That is documented and correct, but it is the kind of thing
a guest gets wrong exactly once — this one did, and the symptom was a guest that
attached second and never learned what its peer had already written. Both guests
now seed their callbacks from `current`, with a comment saying why.
