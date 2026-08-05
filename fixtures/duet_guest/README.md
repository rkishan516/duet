# duet_guest

A tracked fixture, not a published package: the Dart guest client for
[`duet-protocol`](../../crates/duet-protocol), the Rust message envelope a
Flutter engine and a `wry` webview both speak to a Rust host that owns the
shared state ("Duet").

This package plays the same role for a Flutter guest that
`crates/duet-webview/src/bootstrap.rs`'s `window.__duet` JavaScript plays for
a webview guest: it is the thing on the other end of one platform channel,
`duet/rpc` (a `BasicMessageChannel<String>` using `StringCodec` — raw UTF-8,
no envelope), talking to `duet_protocol::handle_text` in
`crates/duet-protocol/src/text.rs` on the Rust side.

## What is here

- `lib/duet_value.dart` — encodes and decodes `duet-codec`'s tagged JSON value
  format (mirrors `crates/duet-codec/src/value.rs`).
- `lib/duet_client.dart` — `DuetClient`: `get`/`set`/`subscribe`/`unsubscribe`
  request/response calls over `kDuetChannel`, plus an `onPush` hook for
  unsolicited host notifications.
- `lib/main.dart` — a **headless** entry point (`WidgetsFlutterBinding`, no
  `runApp`, no widget tree). It is not exercised by `flutter test`; it is what
  a Rust example (`cargo run -p duet-backend-macos --example flutter_state`,
  a later task) boots inside a `FlutterEngine` to drive `DuetClient` against
  the real host.
- `test/duet_client_test.dart` — unit tests against a mocked
  `TestDefaultBinaryMessenger`, including the wire rules that must match
  Rust exactly (canonical decimal `id`/`subscription`/`subscriber` strings,
  `Int` never traveling as a JSON number, and totality of the push handler
  against malformed input).
- `test/rust_goldens_test.dart` — the same round trip, but replayed against
  byte-for-byte output captured from a real Rust host run, so a wire-shape
  regression on either side fails here even if the hand-written unit tests
  above do not exercise that exact byte sequence.

## Building `App.framework`

The Rust example that embeds this guest needs a debug `App.framework`
produced by a normal Flutter macOS build. From the repository root:

```bash
cd fixtures/duet_guest && /Users/kishan/dev/rkishan516/flutterDC/bin/flutter build macos --debug
```

This produces
`fixtures/duet_guest/build/macos/Build/Products/Debug/App.framework`, which
the Rust example expects at `DUET_APP_FRAMEWORK_PATH`:

```bash
DUET_APP_FRAMEWORK_PATH=fixtures/duet_guest/build/macos/Build/Products/Debug/App.framework \
  cargo run -p duet-backend-macos --example flutter_state
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
`pubspec.yaml`, this `README.md`, `lib/`, and `test/` — following the same
precedent `spikes/spike_app/` set in the repository root `.gitignore`. If the
untracked scaffolding is ever missing (a fresh clone, or after `flutter
clean`), regenerate it with:

```bash
flutter create --platforms=macos --org com.example --project-name duet_guest fixtures/duet_guest
```

then `flutter pub get`.
