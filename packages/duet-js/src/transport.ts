/**
 * The seam between the Duet protocol and whatever carries it.
 *
 * Mirrors `packages/duet/lib/src/duet_transport.dart`.
 *
 * @module
 */

/**
 * The transport a {@link DuetClient} speaks over.
 *
 * `./wry.ts` implements this over a `wry` webview's IPC channel; a browser, a
 * websocket host or a test harness can implement it with anything that carries
 * text in both directions. Keeping it abstract is what lets the entire client —
 * and therefore the entire wire format — be tested under plain `node --test`,
 * with no webview and no host process.
 *
 * Text in, text out, and nothing else: `duet_protocol::handle_text(&str) ->
 * String` (crates/duet-protocol/src/text.rs) is already exactly this shape, so
 * the seam adds no translation of its own.
 *
 * An implementation must be **total**: it may resolve {@link send} with `null`
 * and it may deliver malformed text to {@link onPush}, but it must not reject
 * or throw anything outside the transport's own error type into the client.
 * `DuetClient` turns a `null` reply into a `DuetTransportError` and drops a
 * malformed push.
 */
export interface DuetTransport {
  /**
   * Sends one request and resolves with the host's reply, or `null` if no host
   * is listening on the channel.
   *
   * The returned promise must be bound to *this* message's reply, not to
   * whichever reply arrives next: `DuetClient` keeps no pending-request map and
   * relies on the transport to correlate. A transport that cannot do that must
   * build its own correlation on top of the envelope's `id` — which is exactly
   * what the `wry` adapter does, because a webview's IPC channel has no
   * per-message reply path at all.
   */
  send(request: string): Promise<string | null>;

  /**
   * The handler for unsolicited host pushes. `null` removes it.
   *
   * A single mutable slot rather than an event emitter, so that removal is
   * unambiguous and an implementation can map it straight onto a platform
   * channel's own single-handler slot.
   */
  onPush: ((message: string) => void) | null;
}
