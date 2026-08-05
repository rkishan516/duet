/// The seam between the Duet protocol and whatever carries it.
library;

/// The transport a `DuetClient` speaks over.
///
/// `duet_flutter` implements this with a `BasicMessageChannel`; a browser, a
/// websocket host or a test harness can implement it with anything that
/// carries text in both directions. Keeping it abstract is what lets the
/// entire client — and therefore the entire wire format — be tested under
/// plain `dart test`, with no Flutter SDK.
///
/// Text in, text out, and nothing else: `duet_protocol::handle_text(&str) ->
/// String` (crates/duet-protocol/src/text.rs) is already exactly this shape, so
/// the seam adds no translation of its own.
///
/// An implementation must be **total**: it may complete [send] with `null` and
/// it may deliver malformed text to [onPush], but it must not throw anything
/// outside the transport's own error type into the client. `DuetClient` turns
/// a `null` reply into a `DuetTransportException` and drops a malformed push.
abstract interface class DuetTransport {
  /// Sends one request and completes with the host's reply, or `null` if no
  /// host is listening on the channel.
  ///
  /// The returned future must be bound to *this* message's reply, not to
  /// whichever reply arrives next: `DuetClient` keeps no pending-request map
  /// and relies on the transport to correlate. A transport that cannot do that
  /// must build its own correlation on top of the envelope's `id`.
  Future<String?> send(String request);

  /// Installs the handler for unsolicited host pushes. `null` removes it.
  ///
  /// A setter rather than a stream so that removal is unambiguous and so an
  /// implementation can map it straight onto a platform channel's own
  /// single-handler slot.
  set onPush(void Function(String message)? handler);
}
