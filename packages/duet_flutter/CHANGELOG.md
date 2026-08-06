# Changelog

## 0.1.1

- Widen the `duet` constraint to `>=0.1.0 <0.3.0`. `duet` 0.2.0 adds the typed
  runtime as a separate library; nothing in this binding changes, and it works
  against either version.

## 0.1.0

First release. Extracted from the `duet_guest` Flutter fixture, which is now a
consumer of this package rather than a second copy of it.

- `duetRpcChannel`, the `BasicMessageChannel<String>` on `duet/rpc` with a
  `StringCodec` — raw UTF-8, no envelope, so the host can hand the bytes it
  received straight to `duet_protocol::handle_text`.
- `DuetFlutterTransport`, a `DuetTransport` over that channel. Passes a null
  reply through untranslated, so `DuetClient` names the failure; drops a null
  (zero-byte) push rather than forwarding it; answers every push with `''` so
  the host's `send` completes.
- `duet` is re-exported, so a Flutter guest needs one import.
