# duet_guest

A tracked fixture, not a published package: the Flutter guest **driver** for
[`duet-protocol`](../../crates/duet-protocol), the Rust message envelope a
Flutter engine and a `wry` webview both speak to a Rust host that owns the
shared state ("Duet").

This app plays the same role for a Flutter guest that
`crates/duet-webview/src/bootstrap.rs`'s `window.__duet` JavaScript plays for
a webview guest: it is the thing on the other end of one platform channel,
`duet/rpc` (a `BasicMessageChannel<String>` using `StringCodec` — raw UTF-8,
no envelope), talking to `duet_protocol::handle_text` in
`crates/duet-protocol/src/text.rs` on the Rust side.

## It no longer contains a client

The client itself lives in [`packages/duet`](../../packages/duet) (pure Dart:
values, paths, the envelope, `DuetClient` over a `DuetTransport`) and
[`packages/duet_flutter`](../../packages/duet_flutter) (that transport, over the
`duet/rpc` channel). This fixture depends on both — see `pubspec_overrides.yaml`
for why by path — and is now only the drivers.

That is the point of the split. This fixture used to carry its own copy of the
client, and two copies of a wire format is how the copy and the package drift
apart while both keep passing their own tests. What remains here is what the
packages cannot have: code that only makes sense when a real Rust host is on the
other end.

## What is here

- `lib/main.dart` — a **headless** entry point (`WidgetsFlutterBinding`, no
  `runApp`, no widget tree). It is not exercised by `flutter test`; it is what
  a Rust example boots inside a `FlutterEngine` to drive `DuetClient` against
  the real host. It shakes hands with the host and then picks one of the two
  drivers below.
- `lib/guest_support.dart` — what both drivers need: the log prefix, the
  handshake, and the `mode` store path the host writes to choose a driver.
  One `App.framework` carries both drivers because `flutter build macos`
  compiles `lib/main.dart` and a second entry point's output would land at the
  same path and overwrite the first. An absent or unrecognised `mode` selects
  the solo driver, so a host that has never heard of this gets what it always
  got.
- `lib/solo_driver.dart` — the single-guest sequence driven by
  `cargo run -p duet-backend-macos --example flutter_state`: a `set` Rust reads
  back, five doubles compared bit-for-bit, hostile input over the real channel,
  and one push.
- `lib/duet_driver.dart` — the two-guest sequence driven by
  `cargo run -p duet-backend-macos --example two_guests`, where this guest
  shares a process and a store with a live `wry` webview guest. It writes a
  value for the webview to read, reads the webview's value back, subscribes to
  one shared path and one of its own, and publishes every push it receives —
  including the ones it must keep receiving while the webview tries to
  unsubscribe it and is then torn down.
- `test/guest_support_test.dart` — the handshake, which is the only fixture
  logic with a contract of its own. The wire format is tested in
  `packages/duet` against the golden corpus, and the channel seam in
  `packages/duet_flutter`; neither is re-tested here.

The drivers themselves have no `flutter test` coverage and cannot have any:
what they assert is what a real Rust host does, so the three macOS examples
below *are* their test.

## Building `App.framework`

The Rust example that embeds this guest needs a debug `App.framework`
produced by a normal Flutter macOS build. From the repository root:

```bash
cd fixtures/duet_guest && /Users/kishan/dev/rkishan516/flutterDC/bin/flutter build macos --debug
```

This produces
`fixtures/duet_guest/build/macos/Build/Products/Debug/App.framework`, which
both Rust examples expect at `DUET_APP_FRAMEWORK_PATH`:

```bash
export DUET_APP_FRAMEWORK_PATH=fixtures/duet_guest/build/macos/Build/Products/Debug/App.framework
cargo run -p duet-backend-macos --example flutter_state   # this guest alone
cargo run -p duet-backend-macos --example two_guests      # alongside a webview guest
```

Only a debug/JIT build has been exercised; nothing here has been measured
against a release/AOT build.

## Running the tests

```bash
cd fixtures/duet_guest && /Users/kishan/dev/rkishan516/flutterDC/bin/flutter test
```

## Why most of this directory is gitignored

`flutter create --platforms=macos` scaffolds an Xcode project (`macos/`),
IDE metadata (`.idea/`, `*.iml`), and other regeneratable boilerplate
alongside the hand-written Dart. Only the hand-written parts are tracked —
`pubspec.yaml`, `pubspec_overrides.yaml`, this `README.md`, `lib/`, and
`test/` — following the same
precedent `spikes/spike_app/` set in the repository root `.gitignore`. If the
untracked scaffolding is ever missing (a fresh clone, or after `flutter
clean`), regenerate it with:

```bash
flutter create --platforms=macos --org com.example --project-name duet_guest fixtures/duet_guest
```

then `flutter pub get`.
