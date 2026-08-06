# Changelog

## 0.2.0

The typed runtime, in a new `package:duet/typed.dart` library. Additive: the
existing `package:duet/duet.dart` surface is unchanged, and a guest that only
speaks the wire pays nothing for a layer it does not import.

- `duetValueAt` and `duetValueWith` — read and functionally update a value at
  a path. Both iterative and both total: a locally built tree far past the
  wire's depth limit is handled rather than overflowing the stack.
- `duetMergeMirror` — the three-case merge of one host patch into one
  watcher's mirror. A patch may name a path *at*, *below* or *above* the
  watched one, because `Store::set` always sends the path that was written.
  Kept a pure function so it is testable without a client.
- `DuetCodec<T extends Object>` — the seam between a Dart type and a
  `DuetValue`. The non-nullable bound is load-bearing: it is what lets
  `decode` answer `null` for "refused" without colliding with "decoded to
  null".
- `DuetReading` with four arms — `DuetPresent`, `DuetNone`, `DuetAbsent` and
  `DuetMismatch`. A type mismatch is a first-class outcome, not an exception,
  because another guest may write any type to any path and a push has no call
  stack to throw into.
- `DuetRouter` — one owner of the client's push slot, id-keyed routing, a
  bounded early-arrival buffer, a refetch when a patch cannot be merged
  locally, and a resync when a value will not decode.
- `DuetField<T>` and `DuetOptionalField<T>` — typed `get`/`set`/`watch` at one
  fixed path, keeping `None` and "no such path" apart end to end.

## 0.1.0

First release. Extracted from the `duet_guest` Flutter fixture and made
Flutter-free.

- `DuetValue` and the tagged-JSON value codec, mirroring `duet_core::Value`
  and `duet-codec`.
- `DuetPath`, mirroring `duet_core::Path` — including the rule that an index
  may not follow a `.` (`a.[0]` is refused, `a[0]` is not).
- `DuetRequest`, `DuetResponse`, `DuetPush` and the envelope codec, mirroring
  `duet-protocol`.
- `DuetClient` over a new two-member `DuetTransport` interface, replacing the
  fixture's hard dependency on `BasicMessageChannel`. This is what makes the
  package pure Dart, and what puts the whole wire surface behind `dart test`.
- Conformance against `corpus/wire-corpus.json`, the cross-language golden wire
  corpus: all 50 accept cases and all 20 reject cases, with reject cases
  checked against the recorded reason code rather than merely "threw".
- A `maxJsonDepth` guard on decoding. `dart:convert` has no recursion limit and
  will happily build a 100 000-deep tree, where `serde_json` refuses past 128 —
  so without this a Dart guest accepted messages every Rust peer rejects, and
  then handed the result to a recursive decoder.
