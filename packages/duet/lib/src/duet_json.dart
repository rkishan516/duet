/// The JSON layer underneath the Duet wire format: parsing untrusted text,
/// emitting canonical text, and the canonical order of object keys.
library;

import 'dart:convert';

import 'duet_error.dart';

/// Parses one Duet wire message, refusing anything the Rust peer's JSON parser
/// would refuse.
///
/// Three failure modes, all reported as [DuetReason.badJson] so the three
/// implementations agree on the reason as well as the verdict:
///
/// - the text is not JSON, which `jsonDecode` reports as a `FormatException`;
/// - the text nests containers deeper than [maxJsonDepth], which `jsonDecode`
///   accepts and `serde_json` does not;
/// - a string or object key holds an unpaired UTF-16 surrogate, which
///   `jsonDecode` also accepts and `serde_json` also does not.
///
/// Those last two are the reason this function exists rather than a bare
/// `jsonDecode` call, and both are cases where `dart:convert` is *wider* than
/// the wire.
///
/// The depth check must run *before* any of this package's decoders touch the
/// tree, because those decoders recurse: an unbounded tree would overflow the
/// stack on the way down. `jsonDecode` itself is iterative and will not, which
/// is exactly why it cannot be relied on to raise the alarm.
///
/// The surrogate check is there because an unpaired surrogate is not text: it
/// has no UTF-8 encoding at all. `jsonDecode('"\\ud800"')` yields a Dart string
/// holding one, and `utf8.encode` then replaces it with U+FFFD — **silently**,
/// with no error, so the value that reaches the host is not the value that left
/// the guest. `serde_json` refuses the same input outright, so accepting it here
/// would be a divergence that ends in quiet corruption rather than a failure.
///
/// Throws [DuetCodecException] and nothing else, whatever [text] contains.
Object? decodeDuetJson(String text) {
  final Object? parsed;
  try {
    parsed = jsonDecode(text);
  } on FormatException catch (e) {
    throw DuetCodecException.badJson('malformed JSON: ${e.message}');
  }
  _requireWireSafe(parsed);
  return parsed;
}

/// Serializes [json] to the compact text the wire carries.
///
/// Throws [DuetCodecException] with [DuetReason.badJson] if any string or object
/// key holds an unpaired UTF-16 surrogate. `jsonEncode` would happily write one
/// as `\ud800`, which every Rust peer refuses to parse — and `utf8.encode`
/// would turn it into U+FFFD without a word. Refusing here means a caller who
/// built such a string learns about it at the point of the mistake instead of
/// discovering a replacement character on the other side of the wire.
///
/// Key order is *not* imposed here: `jsonEncode` walks a Dart `Map` in
/// insertion order, so it is [sortedJsonObject] — called by every `toJson` in
/// this package — that puts the keys in canonical order. Doing it that way
/// reuses `dart:convert`'s string escaper, which already agrees byte for byte
/// with `serde_json`'s (short escapes for `\b \t \n \f \r`, lowercase
/// `\u00xx` for the other control characters, no escaping of `/` or non-ASCII).
/// A hand-written encoder would have to re-derive all of that correctly.
String encodeDuetJson(Object? json) {
  _requireWireSafe(json, checkDepth: false);
  return jsonEncode(json);
}

/// Rebuilds [fields] with its keys in the wire's canonical order.
///
/// # Why every `toJson` in this package goes through here
///
/// The Rust host serializes through `serde_json::Map`, which is a `BTreeMap`
/// in this workspace, so its object keys come out in byte order —
/// `{"id":"7","kind":"get","path":"a"}`, not the order the fields are declared
/// in. Dart's `jsonEncode` emits insertion order instead. A client that builds
/// `{kind, id, path}` in declaration order therefore produces *equivalent*
/// JSON that is not byte-equal, and fails every byte-exact case in the golden
/// corpus.
///
/// Sorting here rather than at each call site means a field added to an
/// envelope later cannot reintroduce the bug by being appended in the wrong
/// place.
Map<String, Object?> sortedJsonObject(Map<String, Object?> fields) {
  final List<String> keys = fields.keys.toList(growable: false)
    ..sort(compareDuetMapKeys);
  return <String, Object?>{
    for (final String key in keys) key: fields[key],
  };
}

/// Orders two JSON object keys by **code point** — the Duet wire's canonical
/// order, and equivalently UTF-8 byte order (UTF-8 is constructed so that
/// byte-wise comparison of encoded strings agrees with code-point comparison).
/// This is the order Rust's `BTreeMap<String, _>` produces for free, which is
/// why `duet-codec` contains no sorting code at all.
///
/// # Do NOT replace this with `a.compareTo(b)`
///
/// A future maintainer will look at this and see a hand-rolled version of the
/// built-in. It is not. Dart's `String.compareTo` compares **UTF-16 code
/// units**, and those disagree with code points above the BMP:
///
/// - `U+1F600` (😀) is encoded in UTF-16 as the surrogate pair `D83D DE00`.
/// - `0xD83D` is numerically **less than** `0xE000`.
/// - So `compareTo` sorts `U+1F600` *before* `U+E000`, while code-point order —
///   and Rust — sorts it *after*.
///
/// Every key below `U+D800` compares identically under both rules, so a test
/// using ASCII or Latin-1 keys passes either way. That is precisely how this
/// divergence went unnoticed; the corpus case `value/map/code_point_order` and
/// the tests in `test/duet_value_test.dart` use `U+E000`, `U+FFFD` and
/// `U+1F600` deliberately.
///
/// Iterating `runes` is what makes this correct: it yields code points, pairing
/// surrogates back up, whereas indexing a `String` yields code units.
int compareDuetMapKeys(String a, String b) {
  final Iterator<int> aRunes = a.runes.iterator;
  final Iterator<int> bRunes = b.runes.iterator;
  while (true) {
    final bool aHasNext = aRunes.moveNext();
    final bool bHasNext = bRunes.moveNext();
    // One ran out: the shorter string is a prefix of the longer, so it sorts
    // first. Both ran out together means the strings are equal.
    if (!aHasNext) return bHasNext ? -1 : 0;
    if (!bHasNext) return 1;
    final int aRune = aRunes.current;
    final int bRune = bRunes.current;
    if (aRune != bRune) return aRune < bRune ? -1 : 1;
  }
}

/// Walks [root] iteratively, throwing if any container nests past
/// [maxJsonDepth] (when [checkDepth]) or any string holds an unpaired UTF-16
/// surrogate.
///
/// Iterative on purpose: a recursive check would overflow the stack on exactly
/// the input it exists to reject, since `jsonDecode` will have already built
/// the pathological tree without complaint.
///
/// One walk rather than two, because both rules apply to every node and the
/// tree may be large. [checkDepth] is `false` on the encode path: the depth of a
/// document this package *builds* is the caller's own doing rather than
/// untrusted input, and `jsonEncode` is what would fail on it, loudly.
void _requireWireSafe(Object? root, {bool checkDepth = true}) {
  // Each entry is a node paired with the number of containers enclosing it.
  final List<(Object?, int)> pending = <(Object?, int)>[(root, 0)];
  while (pending.isNotEmpty) {
    final (Object? node, int depth) = pending.removeLast();
    if (node is String) {
      _requireNoUnpairedSurrogate(node, 'a string');
      continue;
    }
    if (node is! List<Object?> && node is! Map<String, Object?>) continue;
    final int inner = depth + 1;
    if (checkDepth && inner > maxJsonDepth) {
      throw DuetCodecException.badJson(
        'JSON nests deeper than $maxJsonDepth containers',
      );
    }
    if (node is List<Object?>) {
      for (final Object? child in node) {
        pending.add((child, inner));
      }
    } else if (node is Map<String, Object?>) {
      for (final MapEntry<String, Object?> entry in node.entries) {
        // Keys are text on the wire too, and a `Value::Map` key is chosen by
        // whoever built the value — checking only the values would leave the
        // identical hazard open on the other half of every entry.
        _requireNoUnpairedSurrogate(entry.key, 'an object key');
        pending.add((entry.value, inner));
      }
    }
  }
}

/// Throws [DuetCodecException] if [s] holds an unpaired UTF-16 surrogate.
///
/// [what] names the position ("a string", "an object key"). The offending text
/// is deliberately **not** echoed: it can be arbitrarily long and arrives from a
/// peer, and it holds a code unit that renders as a replacement character
/// anyway, so quoting it would cost an unbounded allocation to print nothing
/// useful.
void _requireNoUnpairedSurrogate(String s, String what) {
  if (!hasUnpairedSurrogate(s)) return;
  throw DuetCodecException.badJson(
    '$what holds an unpaired UTF-16 surrogate, which has no UTF-8 encoding',
  );
}

/// True if [s] contains a UTF-16 surrogate code unit that is not part of a
/// valid high-then-low pair.
///
/// Dart strings are sequences of UTF-16 code units and nothing enforces that
/// surrogates come in pairs, so `String.fromCharCode(0xD800)` is a perfectly
/// constructible `String` that denotes no character at all. It has no UTF-8
/// encoding, `utf8.encode` substitutes U+FFFD for it without raising anything,
/// and `serde_json` rejects the matching wire text — which makes it exactly the
/// kind of value that must not be allowed to cross this boundary in either
/// direction.
///
/// Scans code units rather than runes on purpose: `String.runes` *repairs*
/// unpaired surrogates into U+FFFD as it iterates, so a rune-based check can
/// never see the thing it is looking for.
bool hasUnpairedSurrogate(String s) {
  for (int i = 0; i < s.length; i++) {
    final int unit = s.codeUnitAt(i);
    if (unit < 0xD800 || unit > 0xDFFF) continue;
    // A low surrogate here has no high surrogate before it: the previous
    // iteration would have consumed it as the second half of a pair.
    if (unit >= 0xDC00) return true;
    i++;
    if (i >= s.length) return true;
    final int low = s.codeUnitAt(i);
    if (low < 0xDC00 || low > 0xDFFF) return true;
  }
  return false;
}

/// True if [s] is a canonical, sign-free decimal digit string: at least one
/// digit, and no leading zero unless [s] is exactly `"0"`.
///
/// Mirrors `duet_codec::is_canonical_unsigned_digits`
/// (crates/duet-codec/src/canonical.rs). This is the rule for every
/// **unsigned** integer on the wire — the `id`, `subscription` and `subscriber`
/// fields.
///
/// It matters because those ids are *echoed* by the host in canonical form. A
/// guest that accepted `"007"`, keyed a pending entry by the string it sent,
/// and then received `"7"` back would never match the two — a silent hang with
/// no error at all. `int.tryParse` alone accepts `"007"` and `"+1"`, so this
/// predicate has to gate it.
///
/// Validates *spelling only*: `"18446744073709551615"` is spelled canonically
/// and is still outside the wire's id domain. See `parseWireId` in
/// `duet_message.dart` for the paired range check.
bool isCanonicalUnsignedDigits(String s) {
  if (s.isEmpty) return false;
  for (final int c in s.codeUnits) {
    if (c < 0x30 || c > 0x39) return false;
  }
  return s == '0' || !s.startsWith('0');
}

/// True if [s] is a canonical **signed** decimal string: an optional `-`
/// followed by canonical digits, and never `"-0"`.
///
/// Mirrors `duet_codec::is_canonical_signed_decimal`. This is the rule for
/// `Value::Int` payloads, which — unlike ids — may be negative.
bool isCanonicalSignedDecimal(String s) {
  if (s.startsWith('-')) {
    final String magnitude = s.substring(1);
    return magnitude != '0' && isCanonicalUnsignedDigits(magnitude);
  }
  return isCanonicalUnsignedDigits(s);
}

/// Names a decoded JSON value's type without rendering its contents.
///
/// Mirrors `duet_codec`'s `json_type_name`. Rendering an arbitrary
/// guest-supplied tree into an error message would be an O(n) walk and
/// allocation on the error path for whatever the peer sent — a
/// denial-of-service shape on a hot IPC path. Naming the type is O(1) and
/// still actionable.
String jsonTypeName(Object? json) => switch (json) {
      null => 'null',
      bool _ => 'a boolean',
      num _ => 'a number',
      String _ => 'a string',
      List<Object?> _ => 'an array',
      _ => 'an object',
    };
