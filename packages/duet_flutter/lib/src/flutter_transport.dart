/// The Duet transport, over a Flutter platform channel.
library;

import 'package:duet/duet.dart';
import 'package:flutter/services.dart';

/// The one channel the whole protocol travels on, in both directions.
///
/// Named by [duetChannelName] rather than by a literal: the string lives once,
/// in the transport-agnostic `duet` package, and must equal `DUET_RPC_CHANNEL`
/// in `crates/duet-backend-macos/src/flutter_surface.rs`. Nothing links the two
/// at build time, and a rename on either side produces a guest talking to a
/// channel with no host handler — which surfaces as a *null reply*, not an
/// error, so both sides pin the literal in a test instead.
///
/// # Why `StringCodec`
///
/// [StringCodec] puts the payload on the wire as raw UTF-8 with **no envelope**
/// and no length prefix — it is `utf8.encode`/`utf8.decode` and nothing else.
/// That is exactly why it was chosen: `duet_protocol::handle_text(&str) ->
/// String` (crates/duet-protocol/src/text.rs) *is* the wire shape, so the host
/// hands the bytes it received straight to the protocol and returns the string
/// the protocol produced. A `StandardMessageCodec` or a `JSONMessageCodec`
/// would wrap that text in a second, Flutter-specific encoding that the Rust
/// host would then have to unwrap — a redundant format on the hot path, and one
/// more thing for a non-Flutter guest to reimplement.
///
/// A `BasicMessageChannel`, not a `MethodChannel`, for the same reason: there
/// is no method name to carry. The protocol's own `kind` discriminator is
/// already inside the text.
const BasicMessageChannel<String> duetRpcChannel = BasicMessageChannel<String>(
  duetChannelName,
  StringCodec(),
);

/// A [DuetTransport] over a `BasicMessageChannel<String>`.
///
/// This is the entire Flutter-specific half of a Duet guest: hand one to
/// [DuetClient] and the rest of the protocol comes from the pure-Dart `duet`
/// package.
///
/// ```dart
/// final DuetClient duet = DuetClient(DuetFlutterTransport())..start();
/// await duet.set('editor.zoom', const DuetFloat(1.5));
/// ```
///
/// `final`, because the extension point is [DuetTransport] itself: a transport
/// that wants to log, throttle or fake traffic composes by implementing that
/// interface, which is two members, rather than by inheriting a platform
/// channel's behaviour it would then have to keep consistent.
final class DuetFlutterTransport implements DuetTransport {
  /// Wraps [channel], defaulting to [duetRpcChannel].
  ///
  /// The parameter exists so a test can inject a *distinct* channel instance
  /// rather than several tests fighting over the one real channel's global mock
  /// handler — and so an application embedding two independent hosts can give
  /// each its own channel name.
  DuetFlutterTransport({BasicMessageChannel<String>? channel})
      : channel = channel ?? duetRpcChannel;

  /// The platform channel this transport speaks over.
  ///
  /// Public because raw access is occasionally the point: a conformance driver
  /// that wants to put deliberately malformed text on the wire must bypass
  /// [DuetClient]'s encoder, and [send] is exactly that door. Ordinary
  /// application code should never need this.
  final BasicMessageChannel<String> channel;

  /// Sends one request and completes with the host's reply.
  ///
  /// Completes with `null` — it does **not** throw — when no host handler is
  /// registered on [channel]. This is where `BasicMessageChannel` differs from
  /// `MethodChannel`: the latter throws `MissingPluginException`, the former
  /// hands back a null message, because a null reply is a legal thing for a
  /// handler to send and the channel cannot tell "nobody listened" from
  /// "somebody listened and said nothing".
  ///
  /// Passed through as `null` deliberately, rather than translated here.
  /// [DuetClient] already turns a null reply into a `DuetTransportException`
  /// naming the request it answered, and duplicating that in every transport
  /// would mean every transport gets to phrase it differently.
  @override
  Future<String?> send(String request) => channel.send(request);

  /// Installs the handler for unsolicited host pushes; `null` removes it.
  ///
  /// Maps straight onto the channel's own single-handler slot, so installing a
  /// second handler replaces the first exactly as `setMessageHandler` does.
  ///
  /// Two details the wrapper exists for:
  ///
  /// - A `null` message is dropped rather than forwarded. `StringCodec` decodes
  ///   an empty payload — which is what a host sending zero bytes produces — to
  ///   `null`, and [DuetTransport] promises the handler a non-null `String`.
  /// - The reply is `''`, not the decoded message. A push is fire-and-forget:
  ///   the host is not waiting on an answer, but its `send` does not complete
  ///   until one arrives, so something must be returned. `''` is the cheapest
  ///   thing that is not mistakable for a protocol message.
  @override
  set onPush(void Function(String message)? handler) {
    channel.setMessageHandler(
      handler == null
          ? null
          : (String? message) async {
              if (message != null) handler(message);
              return '';
            },
    );
  }
}
