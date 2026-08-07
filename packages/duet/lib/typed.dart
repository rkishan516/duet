/// The typed runtime: a hand-written layer that turns Duet's untyped value
/// tree into typed fields and typed watchers.
///
/// Import it separately from `package:duet/duet.dart`:
///
/// ```dart
/// import 'package:duet/duet.dart';
/// import 'package:duet/typed.dart';
/// ```
///
/// A separate library, mirroring `duet-protocol/typed` on the JavaScript side,
/// so that a guest that only speaks the wire pays nothing for a layer it does
/// not use.
///
/// # What lives here and why it is hand-written
///
/// Phase 4 generates typed clients from one Rust type definition. Everything
/// the generator will emit is declarations and literals; every *algorithm*
/// those declarations stand on lives here, written and tested by hand:
///
/// - [duetValueAt] and [duetValueWith] — read and functionally update a value
///   at a path, iteratively.
/// - [duetMergeMirror] — the three-case merge of one host patch into one
///   watcher's mirror. The single hardest piece of logic in the layer, kept
///   pure so it can be tested without a client.
/// - [DuetCodec] — the seam between a Dart type and a `DuetValue`.
/// - [DuetReading] and its four arms — what a field currently holds,
///   including the two answers that are not a value and the one that is a
///   disagreement between guests.
/// - [DuetRouter] — one owner of the client's push slot, routing
///   notifications to watchers by subscription id.
/// - [DuetField] and [DuetOptionalField] — typed `get`/`set`/`watch` at one
///   fixed path.
/// - [DuetOutcome] and [duetDecodeOutcome] — the two arms of an `invoke` that
///   ran, decoded through the codecs the command's schema declares, and the
///   third arm for an answer no codec here could read.
///
/// It is useful on its own: a guest with no generated code at all can build
/// fields by hand and get a mirror that stays correct under concurrent writes
/// from another guest.
library;

export 'src/typed/duet_codec.dart';
export 'src/typed/duet_field.dart';
export 'src/typed/duet_merge.dart';
export 'src/typed/duet_outcome.dart';
export 'src/typed/duet_reading.dart';
export 'src/typed/duet_router.dart';
export 'src/typed/duet_value_path.dart';
