# duet_flutter

The Flutter binding for [`duet`](https://pub.dev/packages/duet), the Duet wire
protocol in pure Dart.

Duet is a Rust host that owns shared application state, with guests — a Flutter
engine, a `wry` webview — reading and writing that state over a tagged-JSON
protocol. `duet` is the protocol; this package is the one adapter a Flutter
guest needs to reach a host.

## Using it

```dart
import 'package:duet_flutter/duet_flutter.dart';

final DuetClient duet = DuetClient(DuetFlutterTransport())..start();
duet.onPush = (DuetNotification note) { /* a watched path changed */ };

await duet.set('editor.zoom', const DuetFloat(1.5));
final DuetValue? zoom = await duet.get('editor.zoom');
final DuetSubscription sub = await duet.subscribe('editor');
await duet.unsubscribe(sub.id);
```

One import: this package re-exports `duet`.

## What is actually in here

| Member | What it is |
|---|---|
| `duetRpcChannel` | `BasicMessageChannel<String>('duet/rpc', StringCodec())` |
| `DuetFlutterTransport` | `DuetTransport` over that channel |

That is the whole package, and it is meant to stay that way. Anything that is
not "adapt a channel to an interface" belongs in `duet`.

## Why `StringCodec`

`StringCodec` puts the payload on the wire as **raw UTF-8 with no envelope** and
no length prefix — it is `utf8.encode`/`utf8.decode` and nothing else. That is
exactly why it was chosen: on the host side,

```rust
duet_protocol::handle_text(&str) -> String
```

*is* the wire shape. The host hands the bytes it received straight to the
protocol and returns the string the protocol produced, with no unwrapping in
between.

`StandardMessageCodec` or `JSONMessageCodec` would wrap the same characters in a
second, Flutter-specific encoding that the Rust host would then have to peel off
— a redundant format on a hot IPC path, and one more thing a non-Flutter guest
would have to reimplement to talk to the same host.

For the same reason this is a `BasicMessageChannel` and not a `MethodChannel`:
there is no method name to carry, because the protocol's own `kind`
discriminator is already inside the text.

## Why the channel name lives in `duet`

`duetRpcChannel` is built from `duetChannelName`, which is declared once in the
transport-agnostic package. It must equal `DUET_RPC_CHANNEL` in
`crates/duet-backend-macos/src/flutter_surface.rs`.

Nothing links the two at build time, and a rename on either side does not fail
the build. It produces a guest talking to a channel no host handles — which
surfaces as a null reply, indistinguishable from a slow host. Both sides
therefore pin the literal `duet/rpc` in a test instead.

## The null reply

`BasicMessageChannel.send` completes with `null` when no host handler is
registered. It does **not** throw `MissingPluginException` the way a
`MethodChannel` would, because a null reply is a legal thing for a handler to
send and the channel cannot tell "nobody listened" from "somebody listened and
said nothing".

`DuetFlutterTransport.send` passes that `null` straight through rather than
translating it. `DuetClient` already turns a null reply into a
`DuetTransportException` naming the request it answered; a transport that threw
its own exception here would mean every transport got to phrase the same failure
differently.

The practical consequence for a guest: there is no readiness signal on this
channel. The only handshake available is to send a harmless request and retry
until something answers — which is what
`fixtures/duet_guest/lib/guest_support.dart` does.

## Testing

```
flutter test
```

Only the seam is tested here — the channel name and codec, text crossing
unaltered in both directions, the null-reply behaviour, and push delivery and
totality. The wire format itself is tested in `duet` under plain `dart test`
against the cross-language golden corpus, and re-testing it here would be
precisely the duplication that lets two copies drift apart.

## License

MIT OR Apache-2.0, matching the Rust workspace.
