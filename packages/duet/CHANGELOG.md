# Changelog

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
