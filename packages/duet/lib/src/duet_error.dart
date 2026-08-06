/// Every error this package throws, and the machine-readable reason codes
/// they carry.
///
/// One sealed hierarchy, so a caller can write `on DuetException` and be sure
/// nothing else escapes — and so a test can assert that a malformed message
/// produced *this package's* refusal rather than a `FormatException`, a
/// `TypeError` or a `RangeError` leaking out of a lower layer.
library;

/// The stable, machine-readable names for *why* a wire payload was refused.
///
/// These mirror `duet_codec::CodecError::reason_code`
/// (crates/duet-codec/src/error.rs) one for one. They are part of the
/// cross-language contract, not prose: the golden wire corpus at
/// `corpus/wire-corpus.json` records one per reject case, so the Rust, Dart and
/// JavaScript decoders must agree not merely that a message is bad but on
/// *which rule* it broke. A decoder that rejects everything for one blanket
/// reason passes a "does it throw?" test completely while agreeing with nobody.
abstract final class DuetReason {
  /// The text is not JSON at all, or its nesting exceeds [maxJsonDepth].
  ///
  /// Deliberately absent from Rust's `CodecError`: on that side `serde_json`
  /// refuses such input before any `CodecError` could be constructed, so the
  /// corpus carries the reason even though the enum does not.
  static const String badJson = 'bad_json';

  /// A `"t"` type tag or a `"kind"` envelope discriminator outside the known
  /// set.
  static const String unknownTag = 'unknown_tag';

  /// Well-formed JSON in the wrong shape: a missing field, or a field whose
  /// JSON type is not the one the format requires.
  static const String badShape = 'bad_shape';

  /// An integer payload that is not a canonical decimal string, or is outside
  /// the domain its field admits.
  static const String badInt = 'bad_int';

  /// A float payload that is neither a JSON number nor one of the four
  /// recognised string sentinels.
  static const String badFloat = 'bad_float';

  /// A `Bytes` payload that is not valid base64.
  static const String badBase64 = 'bad_base64';

  /// A path string that does not match the path grammar.
  static const String badPath = 'bad_path';

  /// Every reason code, for tests that assert the set is fully covered.
  static const List<String> all = <String>[
    badJson,
    unknownTag,
    badShape,
    badInt,
    badFloat,
    badBase64,
    badPath,
  ];
}

/// The base of every error this package throws.
///
/// `sealed`, so `switch` over it is exhaustive and a new failure mode cannot be
/// added without every handler being told about it.
sealed class DuetException implements Exception {
  /// What went wrong, safe to show a developer.
  ///
  /// Bounded by [maxEchoChars] wherever it embeds guest- or host-supplied
  /// text — see [echoBounded].
  String get message;
}

/// Malformed data on the wire: the bytes could not be turned into a value, a
/// request, a response or a push.
///
/// Thrown rather than asserted, because this package decodes untrusted input
/// and `assert` is stripped from release builds
/// (`--dart-define=dart.vm.product=true`), which would silently turn a
/// rejection into acceptance.
final class DuetCodecException extends DuetException {
  /// Not JSON, or nested deeper than [maxJsonDepth].
  DuetCodecException.badJson(this.message) : reason = DuetReason.badJson;

  /// An unrecognised `"t"` tag or `"kind"` discriminator.
  DuetCodecException.unknownTag(this.message) : reason = DuetReason.unknownTag;

  /// A missing field, or a field of the wrong JSON type.
  DuetCodecException.badShape(this.message) : reason = DuetReason.badShape;

  /// A non-canonical or out-of-domain integer payload.
  DuetCodecException.badInt(this.message) : reason = DuetReason.badInt;

  /// A float payload that is neither a number nor a known sentinel.
  DuetCodecException.badFloat(this.message) : reason = DuetReason.badFloat;

  /// A `Bytes` payload that is not valid base64.
  DuetCodecException.badBase64(this.message) : reason = DuetReason.badBase64;

  /// A path string outside the path grammar.
  DuetCodecException.badPath(this.message) : reason = DuetReason.badPath;

  /// One of [DuetReason]'s codes: which rule the payload broke.
  final String reason;

  @override
  final String message;

  @override
  String toString() => 'DuetCodecException($reason): $message';
}

/// The exchange failed below or beside the protocol: no reply arrived, or a
/// well-formed response answered a different request than the one sent.
///
/// Distinct from [DuetCodecException], which means the bytes themselves were
/// malformed, and from [DuetFailure], which means the host understood the
/// request perfectly and refused it.
final class DuetTransportException extends DuetException {
  /// Creates a transport failure with a developer-facing explanation.
  DuetTransportException(this.message);

  @override
  final String message;

  @override
  String toString() => 'DuetTransportException: $message';
}

/// The host answered `{"kind":"failed", ...}` — a well-formed rejection of a
/// well-formed request, for example a `set` at an unresolvable path.
///
/// The request reached the host and was understood; only the operation failed.
final class DuetFailure extends DuetException {
  /// Creates a failure answering [requestId] with the host's [message].
  DuetFailure(this.requestId, this.message);

  /// The request this failure answers.
  final int requestId;

  @override
  final String message;

  @override
  String toString() => 'DuetFailure(request $requestId): $message';
}

/// The most characters of guest- or host-supplied text any error message may
/// embed, mirroring `duet_codec::error::MAX_ECHO_CHARS`
/// (crates/duet-codec/src/error.rs).
///
/// Without a bound, a one-megabyte payload becomes a one-megabyte log line on
/// a hot IPC path — a denial-of-service shape reachable by anything that can
/// send this client a message.
const int maxEchoChars = 48;

/// Renders [text] quoted and truncated to [maxEchoChars], for embedding in an
/// error message.
String echoBounded(String text) => text.length <= maxEchoChars
    ? '"$text"'
    : '"${text.substring(0, maxEchoChars)}…"';

/// The most nested JSON containers a Duet message may contain: **127**.
///
/// Mirrors `duet_codec::MAX_JSON_DEPTH` (crates/duet-codec/src/depth.rs), which
/// the host enforces itself over raw text before parsing it. The golden corpus
/// pins the boundary in every language at once: `value/nesting/at_limit` has
/// exactly 127 containers and must decode, `value/nesting/over_limit` has 128
/// and must be refused as [DuetReason.badJson].
///
/// # This number must equal the host's exactly, not approximately
///
/// This package shipped with `128` here against a host that stops at 127, so a
/// document with exactly 128 containers was accepted by Dart and refused by
/// Rust. Nothing caught it: the only nesting test in the suite used 200 levels,
/// which every implementation refuses whatever its off-by-one. A limit is only
/// tested by a case on each side of it.
///
/// Dart needs it stated explicitly at all because `dart:convert`'s parser has no
/// limit: `jsonDecode` happily builds a 100 000-deep tree (it is iterative, so
/// it does not overflow the stack building one). Without this guard a Dart guest
/// would accept messages every Rust peer rejects, and — worse — would then hand
/// that tree to this package's recursive decoders, which would overflow the
/// stack for real.
const int maxJsonDepth = 127;
