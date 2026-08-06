/// The Duet message envelope: requests a guest sends, responses the host
/// answers with, and pushes the host originates.
///
/// Mirrors `duet_protocol::{Request, Response, Push}`
/// (crates/duet-protocol/src/message.rs) and their codec
/// (crates/duet-protocol/src/wire.rs).
library;

import 'duet_error.dart';
import 'duet_json.dart';
import 'duet_path.dart';
import 'duet_value.dart';

/// The inclusive top of the Duet wire's id domain, mirroring
/// `duet_codec::MAX_WIRE_ID` (crates/duet-codec/src/canonical.rs).
///
/// This is `i64::MAX`, not `u64::MAX`, **because of Dart**: Dart's native `int`
/// is 64-bit *signed*, so `int.tryParse('9223372036854775808')` returns null
/// and no Dart guest could read a larger id at all. Rather than widen Dart to
/// `BigInt` for a range that ids — allocated sequentially from 1 — never reach,
/// the wire domain itself stops here, so one id domain holds in every guest
/// language.
const int maxWireId = 9223372036854775807;

/// The one channel name every Duet guest and host must agree on.
///
/// Defined here once, in the transport-agnostic package, so a Flutter binding,
/// a websocket host and a test harness all name the same string by reference
/// rather than retyping a literal. `duet/rpc`, not `duet/protocol`.
const String duetChannelName = 'duet/rpc';

/// Parses a wire id, enforcing both halves of the wire's id rule exactly as
/// `duet_codec::parse_wire_id` does. Returns null if either half fails.
///
/// **Canonical spelling** (no leading `+`, no leading zeros): `int.tryParse`
/// alone is too lenient — it accepts `"007"`, which the Rust side rejects.
/// Without this check a Dart guest would accept ids a Rust guest refuses, so
/// the two guests would disagree about what is even well-formed.
///
/// **Domain `0..=`[maxWireId]**: checked explicitly rather than left to
/// `int.tryParse` returning null. On the Dart VM the two happen to coincide,
/// but relying on that would make a shared wire rule an accident of the
/// platform's integer width — and it does not hold everywhere Dart runs:
/// compiled to JavaScript, `int` is a double and `int.tryParse` rounds instead
/// of failing.
int? tryParseWireId(String s) {
  if (!isCanonicalUnsignedDigits(s)) return null;
  final int? parsed = int.tryParse(s);
  if (parsed == null || parsed < 0 || parsed > maxWireId) return null;
  return parsed;
}

/// A guest-to-host message.
sealed class DuetRequest {
  const DuetRequest(this.id);

  /// The correlation id. Monotonic per connection, per the contract on
  /// `RequestId` (crates/duet-protocol/src/message.rs).
  final int id;

  /// The wire form, keys already in canonical order.
  Map<String, Object?> toJson();

  /// This request as the exact text the wire carries.
  String toWireText() => encodeDuetJson(toJson());

  /// Decodes a request from wire text.
  ///
  /// Throws [DuetCodecException] and nothing else, whatever [text] contains.
  static DuetRequest fromWireText(String text) =>
      fromJson(decodeDuetJson(text));

  /// Decodes a request from already-parsed JSON.
  ///
  /// The `id` is read before the `kind` is dispatched on, matching
  /// `duet_protocol::decode_request`: a message with both a bad id and an
  /// unknown kind is reported as a bad id, in both languages.
  static DuetRequest fromJson(Object? json) {
    final Map<String, Object?> obj = _asObject(json, 'request');
    final int id = _idField(obj, 'id');
    return switch (_kind(obj)) {
      'get' => DuetGetRequest(id: id, path: _pathField(obj)),
      'set' => DuetSetRequest(
          id: id,
          path: _pathField(obj),
          value: DuetValue.fromJson(_field(obj, 'value')),
        ),
      'subscribe' => DuetSubscribeRequest(id: id, path: _pathField(obj)),
      'unsubscribe' => DuetUnsubscribeRequest(
          id: id,
          subscription: _idField(obj, 'subscription'),
        ),
      final String other => throw DuetCodecException.unknownTag(
          'unknown request kind ${echoBounded(other)}',
        ),
    };
  }
}

/// Reads the value at [path].
final class DuetGetRequest extends DuetRequest {
  /// Creates a `get` for [path].
  const DuetGetRequest({required int id, required this.path}) : super(id);

  /// The path to read.
  final DuetPath path;

  @override
  Map<String, Object?> toJson() => sortedJsonObject(<String, Object?>{
        'kind': 'get',
        'id': id.toString(),
        'path': path.toString(),
      });

  @override
  String toString() => 'Get(id: $id, path: $path)';
}

/// Writes [value] at [path].
final class DuetSetRequest extends DuetRequest {
  /// Creates a `set` of [value] at [path].
  const DuetSetRequest({
    required int id,
    required this.path,
    required this.value,
  }) : super(id);

  /// The path to write.
  final DuetPath path;

  /// The value to write there.
  final DuetValue value;

  @override
  Map<String, Object?> toJson() => sortedJsonObject(<String, Object?>{
        'kind': 'set',
        'id': id.toString(),
        'path': path.toString(),
        'value': value.toJson(),
      });

  @override
  String toString() => 'Set(id: $id, path: $path, value: $value)';
}

/// Starts watching [path].
final class DuetSubscribeRequest extends DuetRequest {
  /// Creates a `subscribe` for [path].
  const DuetSubscribeRequest({required int id, required this.path}) : super(id);

  /// The path to watch.
  final DuetPath path;

  @override
  Map<String, Object?> toJson() => sortedJsonObject(<String, Object?>{
        'kind': 'subscribe',
        'id': id.toString(),
        'path': path.toString(),
      });

  @override
  String toString() => 'Subscribe(id: $id, path: $path)';
}

/// Stops watching a subscription this guest opened.
///
/// A guest may only cancel its own subscriptions; the host scopes the
/// cancellation to the sender.
final class DuetUnsubscribeRequest extends DuetRequest {
  /// Creates an `unsubscribe` for [subscription].
  const DuetUnsubscribeRequest({required int id, required this.subscription})
      : super(id);

  /// The subscription handed back by the matching `subscribed` response.
  final int subscription;

  @override
  Map<String, Object?> toJson() => sortedJsonObject(<String, Object?>{
        'kind': 'unsubscribe',
        'id': id.toString(),
        'subscription': subscription.toString(),
      });

  @override
  String toString() => 'Unsubscribe(id: $id, subscription: $subscription)';
}

/// A host-to-guest reply to exactly one [DuetRequest].
sealed class DuetResponse {
  const DuetResponse(this.id);

  /// The id of the request this answers.
  final int id;

  /// The wire form, keys already in canonical order.
  Map<String, Object?> toJson();

  /// This response as the exact text the wire carries.
  String toWireText() => encodeDuetJson(toJson());

  /// Decodes a response from wire text.
  ///
  /// Throws [DuetCodecException] and nothing else, whatever [text] contains.
  static DuetResponse fromWireText(String text) =>
      fromJson(decodeDuetJson(text));

  /// Decodes a response from already-parsed JSON.
  static DuetResponse fromJson(Object? json) {
    final Map<String, Object?> obj = _asObject(json, 'response');
    final int id = _idField(obj, 'id');
    return switch (_kind(obj)) {
      'value' => DuetValueResponse(
          id: id,
          value: DuetValue.optionalFromJson(_field(obj, 'value')),
        ),
      'done' => DuetDoneResponse(id: id),
      'subscribed' => DuetSubscribedResponse(
          id: id,
          subscription: _idField(obj, 'subscription'),
          snapshot: DuetValue.optionalFromJson(_field(obj, 'snapshot')),
        ),
      'failed' => DuetFailedResponse(
          id: id,
          message: _stringField(obj, 'message'),
        ),
      final String other => throw DuetCodecException.unknownTag(
          'unknown response kind ${echoBounded(other)}',
        ),
    };
  }
}

/// The answer to a `get`.
final class DuetValueResponse extends DuetResponse {
  /// Creates a `value` response carrying [value].
  const DuetValueResponse({required int id, required this.value}) : super(id);

  /// `null` means the path does not exist — distinct from [DuetNull], which
  /// means it exists and holds null.
  final DuetValue? value;

  @override
  Map<String, Object?> toJson() => sortedJsonObject(<String, Object?>{
        'kind': 'value',
        'id': id.toString(),
        'value': value?.toJson(),
      });

  @override
  String toString() => 'Value(id: $id, value: $value)';
}

/// The answer to a `set` or an `unsubscribe`: it worked.
final class DuetDoneResponse extends DuetResponse {
  /// Creates a `done` response.
  const DuetDoneResponse({required int id}) : super(id);

  @override
  Map<String, Object?> toJson() => sortedJsonObject(<String, Object?>{
        'kind': 'done',
        'id': id.toString(),
      });

  @override
  String toString() => 'Done(id: $id)';
}

/// The answer to a `subscribe`.
final class DuetSubscribedResponse extends DuetResponse {
  /// Creates a `subscribed` response.
  const DuetSubscribedResponse({
    required int id,
    required this.subscription,
    required this.snapshot,
  }) : super(id);

  /// The handle to pass to [DuetUnsubscribeRequest] to cancel.
  final int subscription;

  /// The watched path's value at subscription time, or `null` if the path does
  /// not exist — distinct from [DuetNull].
  final DuetValue? snapshot;

  @override
  Map<String, Object?> toJson() => sortedJsonObject(<String, Object?>{
        'kind': 'subscribed',
        'id': id.toString(),
        'subscription': subscription.toString(),
        'snapshot': snapshot?.toJson(),
      });

  @override
  String toString() =>
      'Subscribed(id: $id, subscription: $subscription, snapshot: $snapshot)';
}

/// A well-formed rejection of a well-formed request.
final class DuetFailedResponse extends DuetResponse {
  /// Creates a `failed` response carrying the host's [message].
  const DuetFailedResponse({required int id, required this.message})
      : super(id);

  /// The host's explanation, safe to show a developer.
  final String message;

  @override
  Map<String, Object?> toJson() => sortedJsonObject(<String, Object?>{
        'kind': 'failed',
        'id': id.toString(),
        'message': message,
      });

  @override
  String toString() => 'Failed(id: $id, message: $message)';
}

/// An unsolicited host-to-guest message, answering no request.
sealed class DuetPush {
  const DuetPush();

  /// The wire form, keys already in canonical order.
  Map<String, Object?> toJson();

  /// This push as the exact text the wire carries.
  String toWireText() => encodeDuetJson(toJson());

  /// Decodes a push from wire text.
  ///
  /// Throws [DuetCodecException] and nothing else, whatever [text] contains.
  static DuetPush fromWireText(String text) => fromJson(decodeDuetJson(text));

  /// Decodes a push from already-parsed JSON.
  static DuetPush fromJson(Object? json) {
    final Map<String, Object?> obj = _asObject(json, 'push');
    return switch (_kind(obj)) {
      'notification' => DuetNotificationPush(
          DuetNotification.fromJson(_field(obj, 'notification')),
        ),
      final String other => throw DuetCodecException.unknownTag(
          'unknown push kind ${echoBounded(other)}',
        ),
    };
  }
}

/// A watched path changed.
final class DuetNotificationPush extends DuetPush {
  /// Wraps [notification].
  const DuetNotificationPush(this.notification);

  /// What changed, and for whom.
  final DuetNotification notification;

  @override
  Map<String, Object?> toJson() => sortedJsonObject(<String, Object?>{
        'kind': 'notification',
        'notification': notification.toJson(),
      });

  @override
  String toString() => 'Push($notification)';
}

/// One change to a watched path.
///
/// Mirrors `duet_core::Notification`, with its `Patch` flattened: the wire
/// nests `{path, value}` under a `patch` object, but a guest only ever wants
/// the two fields, so the nesting stays in [toJson] and [fromJson] rather than
/// becoming a second type for callers to unwrap.
final class DuetNotification {
  /// Creates a notification.
  const DuetNotification({
    required this.subscriber,
    required this.subscription,
    required this.path,
    required this.value,
  });

  /// The host's own `SubscriberId`. Informational: a guest never names one,
  /// since `subscribe` cannot carry it
  /// (crates/duet-protocol/src/message.rs).
  final int subscriber;

  /// Which of this guest's `subscribe` calls this notification answers.
  final int subscription;

  /// The path that changed.
  final DuetPath path;

  /// The path's new value.
  final DuetValue value;

  /// The wire form, keys already in canonical order.
  Map<String, Object?> toJson() => sortedJsonObject(<String, Object?>{
        'subscriber': subscriber.toString(),
        'subscription': subscription.toString(),
        'patch': sortedJsonObject(<String, Object?>{
          'path': path.toString(),
          'value': value.toJson(),
        }),
      });

  /// Decodes a notification from already-parsed JSON.
  static DuetNotification fromJson(Object? json) {
    final Map<String, Object?> obj = _asObject(json, 'notification');
    final Map<String, Object?> patch = _asObject(_field(obj, 'patch'), 'patch');
    return DuetNotification(
      subscriber: _idField(obj, 'subscriber'),
      subscription: _idField(obj, 'subscription'),
      path: DuetPath.parse(_stringField(patch, 'path')),
      value: DuetValue.fromJson(_field(patch, 'value')),
    );
  }

  @override
  String toString() =>
      'Notification(subscriber: $subscriber, subscription: $subscription, '
      'path: $path, value: $value)';
}

/// Casts [json] to an object, or refuses it by name.
Map<String, Object?> _asObject(Object? json, String what) {
  if (json is! Map<String, Object?>) {
    throw DuetCodecException.badShape(
      '$what must be an object, found ${jsonTypeName(json)}',
    );
  }
  return json;
}

/// Reads a required field.
Object? _field(Map<String, Object?> obj, String name) {
  if (!obj.containsKey(name)) {
    throw DuetCodecException.badShape('missing "$name"');
  }
  return obj[name];
}

/// Reads the envelope's `kind` discriminator.
String _kind(Map<String, Object?> obj) => _stringField(obj, 'kind');

/// Reads a required string field.
String _stringField(Map<String, Object?> obj, String name) {
  final Object? raw = _field(obj, name);
  if (raw is! String) {
    throw DuetCodecException.badShape('"$name" must be a string');
  }
  return raw;
}

/// Reads a required path field.
DuetPath _pathField(Map<String, Object?> obj) =>
    DuetPath.parse(_stringField(obj, 'path'));

/// Reads one of the envelope's id fields through the single definition of the
/// wire's id rule, [tryParseWireId].
///
/// Gates every id the envelope carries: `id` on requests and responses,
/// `subscription` on `unsubscribe` and `subscribed`, and both `subscriber` and
/// `subscription` on a notification.
int _idField(Map<String, Object?> obj, String name) {
  final Object? raw = _field(obj, name);
  if (raw is! String) {
    throw DuetCodecException.badShape('"$name" must be a decimal string');
  }
  final int? parsed = tryParseWireId(raw);
  if (parsed == null) {
    throw DuetCodecException.badInt(
      '"$name" is not a canonical decimal string in 0..$maxWireId: '
      '${echoBounded(raw)}',
    );
  }
  return parsed;
}
