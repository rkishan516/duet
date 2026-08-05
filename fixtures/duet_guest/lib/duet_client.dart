// The Dart guest client. Mirrors crates/duet-webview/src/bootstrap.rs's
// `window.__duet`, over a Flutter platform channel instead of wry IPC.
import 'dart:convert';

import 'package:flutter/services.dart';

import 'duet_value.dart';

/// The one channel the whole protocol travels on, in both directions.
///
/// `StringCodec` puts the payload on the wire as raw UTF-8 with no envelope
/// (packages/flutter/lib/src/services/message_codecs.dart:44-63), which is
/// exactly the shape `duet_protocol::handle_text(&str) -> String`
/// (crates/duet-protocol/src/text.rs:24-35) already has, and exactly the
/// primitive `FlutterEngine::set_lifecycle_state`
/// (crates/duet-backend-macos/src/engine.rs:419-433) already drives.
///
/// Named `duet/rpc`, not `duet/protocol`: this is the one and only channel
/// name every guest and every test must agree on, so it is defined here once
/// — as [kDuetChannel] — and used everywhere else by reference, never
/// retyped as a string literal.
const BasicMessageChannel<String> kDuetChannel = BasicMessageChannel<String>(
  'duet/rpc',
  StringCodec(),
);

/// A host push. Mirrors `duet_protocol::Push::Notification`
/// (crates/duet-protocol/src/message.rs:125-128).
class DuetNotification {
  const DuetNotification({
    required this.subscriber,
    required this.subscription,
    required this.path,
    required this.value,
  });

  /// The host's own SubscriberId. Informational: a guest never names one
  /// (see `Request::Subscribe`, message.rs:41-50).
  final int subscriber;

  /// Which of this guest's `subscribe` calls this notification answers.
  final int subscription;

  /// The path that changed.
  final String path;

  /// The path's new value.
  final DuetValue value;

  @override
  String toString() =>
      'Notification(subscriber: $subscriber, subscription: $subscription, '
      'path: $path, value: $value)';
}

/// The answer to `subscribe`. Mirrors `Response::Subscribed`
/// (crates/duet-protocol/src/message.rs:88-95).
class DuetSubscription {
  const DuetSubscription({required this.id, required this.snapshot});

  /// Pass this to [DuetClient.unsubscribe] to cancel.
  final int id;

  /// `null` means the watched path does not exist — distinct from
  /// `DuetNull()`, which means it exists and holds null.
  final DuetValue? snapshot;
}

/// The host answered `{"kind":"failed", ...}` — a well-formed rejection of a
/// well-formed request (for example, a `set` at an unresolvable path).
class DuetFailure implements Exception {
  DuetFailure(this.requestId, this.message);

  /// The request this failure answers.
  final int requestId;

  /// The host's explanation, safe to show a developer.
  final String message;

  @override
  String toString() => 'DuetFailure(request $requestId): $message';
}

/// The transport itself failed — no reply, or a reply that is not a
/// well-formed response. Distinct from [DuetFailure]: this means the
/// exchange never reached the protocol layer at all.
class DuetTransportException implements Exception {
  DuetTransportException(this.message);

  /// What went wrong, safe to show a developer.
  final String message;
  @override
  String toString() => 'DuetTransportException: $message';
}

/// `window.__duet`, in Dart.
class DuetClient {
  /// [channel] defaults to [kDuetChannel]; the parameter exists so tests can
  /// inject a distinct channel instance rather than fighting over the one
  /// real channel's global mock handler.
  DuetClient({BasicMessageChannel<String>? channel})
    : _channel = channel ?? kDuetChannel;

  final BasicMessageChannel<String> _channel;

  /// Monotonic, per the contract on `RequestId`
  /// (crates/duet-protocol/src/message.rs:5-15). NOT used to correlate
  /// replies — see `_call`.
  int _nextId = 1;

  /// The analogue of `window.__duet.onPush`. Unsolicited host->guest traffic.
  void Function(DuetNotification note)? onPush;

  /// Installs the push handler. Nothing arrives until this runs — the exact
  /// same silent-failure shape as a webview guest that never defines
  /// `window.__duet.onPush`.
  void start() => _channel.setMessageHandler(_handleHostMessage);

  /// Uninstalls the push handler. Safe to call even if [start] never ran.
  void stop() => _channel.setMessageHandler(null);

  /// Reads the value at [path]. `null` means the path does not exist.
  Future<DuetValue?> get(String path) async {
    final Map<String, Object?> reply = await _call(<String, Object?>{
      'kind': 'get',
      'path': path,
    });
    _expectKind(reply, 'value');
    return DuetValue.optionalFromJson(reply['value']);
  }

  /// Writes [value] at [path].
  Future<void> set(String path, DuetValue value) async {
    final Map<String, Object?> reply = await _call(<String, Object?>{
      'kind': 'set',
      'path': path,
      'value': value.toJson(),
    });
    _expectKind(reply, 'done');
  }

  /// Watches [path]. The host supplies the SubscriberId; this request cannot
  /// name one (crates/duet-protocol/src/message.rs:41-50).
  Future<DuetSubscription> subscribe(String path) async {
    final Map<String, Object?> reply = await _call(<String, Object?>{
      'kind': 'subscribe',
      'path': path,
    });
    _expectKind(reply, 'subscribed');
    return DuetSubscription(
      id: _u64Field(reply, 'subscription'),
      snapshot: DuetValue.optionalFromJson(reply['snapshot']),
    );
  }

  /// Stops watching a subscription returned by [subscribe].
  Future<void> unsubscribe(int subscription) async {
    final Map<String, Object?> reply = await _call(<String, Object?>{
      'kind': 'unsubscribe',
      'subscription': subscription.toString(),
    });
    _expectKind(reply, 'done');
  }

  /// Sends one request and returns the response that answers it.
  ///
  /// There is no pending-request map here, unlike the JS client
  /// (crates/duet-webview/src/bootstrap.rs:15,23-29): `BasicMessageChannel.send`
  /// returns a Future bound to *this* message's reply
  /// (packages/flutter/lib/src/services/platform_channel.dart:240-242), so the
  /// transport does the correlating. The `id` still travels because
  /// `duet_protocol::decode_request` requires it
  /// (crates/duet-protocol/src/wire.rs:82) and because the webview guest,
  /// which has no reply channel at all, correlates by nothing else.
  Future<Map<String, Object?>> _call(Map<String, Object?> body) async {
    final int id = _nextId++;
    final String request = jsonEncode(<String, Object?>{
      ...body,
      // A decimal string: u64 exceeds JavaScript's safe integer range
      // (crates/duet-protocol/src/wire.rs:14).
      'id': id.toString(),
    });

    final String? text = await _channel.send(request);
    // Unlike MethodChannel, a BasicMessageChannel with no host handler
    // registered completes with null rather than throwing
    // MissingPluginException. Treated as a transport failure, not as silence.
    if (text == null) {
      throw DuetTransportException(
        'no host handler on "${_channel.name}" (null reply to request $id)',
      );
    }

    final Map<String, Object?> reply = _decodeObject(text);
    final int echoed = _u64Field(reply, 'id');
    if (echoed != id) {
      throw DuetTransportException(
        'the host answered request $echoed on the reply to request $id',
      );
    }
    if (reply['kind'] == 'failed') {
      final Object? message = reply['message'];
      throw DuetFailure(id, message is String ? message : '<no message>');
    }
    return reply;
  }

  /// Every unsolicited host->guest message. Responses never arrive here —
  /// they come back as the reply to `send`.
  ///
  /// Total against malformed input by construction: the entire body runs
  /// under `try`/`on Object catch`, so no shape of bad JSON — wrong types,
  /// missing fields, a non-object top level — can throw out of this method
  /// and take the isolate down. A push is fire-and-forget from the host's
  /// perspective; there is no request id to fail against, so the only
  /// sound response to malformed push data is to drop it silently.
  Future<String> _handleHostMessage(String? text) async {
    if (text == null) return '';
    try {
      final Map<String, Object?> push = _decodeObject(text);
      if (push['kind'] != 'notification') {
        throw DuetCodecException('unknown push kind ${push['kind']}');
      }
      final Object? raw = push['notification'];
      if (raw is! Map<String, Object?>) {
        throw DuetCodecException('"notification" must be an object');
      }
      final Object? patch = raw['patch'];
      if (patch is! Map<String, Object?>) {
        throw DuetCodecException('"patch" must be an object');
      }
      final Object? path = patch['path'];
      if (path is! String) throw DuetCodecException('"path" must be a string');
      onPush?.call(
        DuetNotification(
          subscriber: _u64Field(raw, 'subscriber'),
          subscription: _u64Field(raw, 'subscription'),
          path: path,
          value: DuetValue.fromJson(patch['value']),
        ),
      );
    } on Object catch (_) {
      // Total: a malformed push must not take the isolate down. The reply is
      // still sent so the host's send completes.
      return '';
    }
    return '';
  }
}

/// Parses [text] as a JSON object, wrapping every failure mode — invalid
/// JSON, or valid JSON that is not an object — in [DuetTransportException]
/// rather than leaking a raw [FormatException] or [TypeError] to callers.
Map<String, Object?> _decodeObject(String text) {
  final Object? parsed;
  try {
    parsed = jsonDecode(text);
  } on FormatException catch (e) {
    throw DuetTransportException('malformed JSON from the host: ${e.message}');
  }
  if (parsed is! Map<String, Object?>) {
    throw DuetTransportException('the host sent a non-object message');
  }
  return parsed;
}

/// Reads a u64 carried as a CANONICAL decimal string
/// (crates/duet-codec/src/wire.rs:43-52 — no leading `+`, no leading zeros).
///
/// `int.tryParse` alone is too lenient: it accepts "007", which the Rust side
/// rejects. Without this check a Dart guest would accept ids a Rust guest
/// refuses, i.e. the two guests would disagree about what is well-formed.
int _u64Field(Map<String, Object?> obj, String name) {
  final Object? raw = obj[name];
  if (raw is! String) {
    throw DuetTransportException('"$name" must be a decimal string');
  }
  if (!isCanonicalUnsignedDigits(raw)) {
    throw DuetTransportException('"$name" is not a canonical decimal string');
  }
  final int? parsed = int.tryParse(raw);
  if (parsed == null || parsed < 0) {
    throw DuetTransportException('"$name" overflows u64');
  }
  return parsed;
}

/// Checks a decoded reply's `"kind"` against what the caller that sent the
/// request expects, so a shape mismatch fails at the call site that can name
/// what it wanted, rather than surfacing later as a confusing cast error.
void _expectKind(Map<String, Object?> reply, String want) {
  if (reply['kind'] != want) {
    throw DuetTransportException(
      'expected a "$want" response, got "${reply['kind']}"',
    );
  }
}
