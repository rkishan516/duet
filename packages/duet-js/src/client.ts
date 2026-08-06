/**
 * The guest client: `window.__duet`, typed.
 *
 * Mirrors `packages/duet/lib/src/duet_client.dart`.
 *
 * @module
 */

import { DuetError, DuetFailure, DuetTransportError } from './errors.ts';
import {
  decodePushText,
  decodeResponseText,
  encodeRequestText,
  type DuetNotification,
  type DuetRequest,
  type DuetResponse,
} from './message.ts';
import { parseDuetPath } from './path.ts';
import type { DuetTransport } from './transport.ts';
import type { DuetValue } from './value.ts';

/**
 * What {@link DuetClient.subscribe} hands back.
 *
 * A result type of its own rather than the raw subscribed response, which
 * carries *two* ids — the request's and the subscription's. Only the second one
 * matters to a caller, and a caller that reached for `.id` on the response
 * would get the wrong one with no type error to stop it.
 */
export interface DuetSubscription {
  /** Pass this to {@link DuetClient.unsubscribe} to cancel. */
  readonly id: bigint;
  /**
   * The watched path's value at subscription time. `null` means the path does
   * not exist — distinct from a value of kind `'null'`, which means it exists
   * and holds null.
   */
  readonly snapshot: DuetValue | null;
}

/**
 * A guest's handle on the host's shared state.
 *
 * One instance owns one id sequence, so one client per transport.
 */
export class DuetClient {
  /** The transport this client speaks over. */
  readonly transport: DuetTransport;

  /**
   * The analogue of `window.__duet.onPush`: unsolicited host-to-guest traffic.
   *
   * Assigning this is not enough on its own; nothing arrives until
   * {@link start} installs the handler on the transport.
   */
  onPush: ((note: DuetNotification) => void) | null = null;

  /**
   * Monotonic, per the contract on `RequestId`
   * (crates/duet-protocol/src/message.rs). **Not** used to correlate replies —
   * see {@link call}.
   *
   * `bigint`, so the counter shares one domain with the wire's ids rather than
   * silently losing precision past 2^53 the way a `number` would.
   */
  #nextId = 1n;

  /**
   * Wraps `transport`.
   *
   * The transport is injected rather than constructed here: it is the only
   * thing standing between this class and a webview dependency, and taking it
   * as a parameter is what lets a test drive the whole protocol with a fake.
   */
  constructor(transport: DuetTransport) {
    this.transport = transport;
  }

  /**
   * Starts listening for pushes.
   *
   * Nothing arrives until this runs — the same silent-failure shape as a
   * webview guest that never defines `window.__duet.onPush`.
   */
  start(): void {
    this.transport.onPush = (message: string): void => {
      this.#handleHostMessage(message);
    };
  }

  /** Stops listening for pushes. Safe to call even if {@link start} never ran. */
  stop(): void {
    this.transport.onPush = null;
  }

  /**
   * Reads the value at `path`. `null` means the path does not exist, which is
   * distinct from a path that exists and holds a value of kind `'null'`.
   *
   * @throws DuetCodecError if `path` is not a legal path — before anything is
   *   sent, so a typo costs no round trip.
   * @throws DuetFailure if the host refuses.
   * @throws DuetTransportError if the exchange never reached the protocol.
   */
  async get(path: string): Promise<DuetValue | null> {
    const reply = await this.#call({ kind: 'get', id: this.#nextId++, path: parseDuetPath(path) });
    return this.#expect(reply, 'value').value;
  }

  /** Writes `value` at `path`. */
  async set(path: string, value: DuetValue): Promise<void> {
    const reply = await this.#call({
      kind: 'set',
      id: this.#nextId++,
      path: parseDuetPath(path),
      value,
    });
    this.#expect(reply, 'done');
  }

  /**
   * Starts watching `path`, returning the host's snapshot and the handle to
   * cancel with.
   *
   * The host allocates the `SubscriberId`; this request cannot name one.
   */
  async subscribe(path: string): Promise<DuetSubscription> {
    const reply = await this.#call({
      kind: 'subscribe',
      id: this.#nextId++,
      path: parseDuetPath(path),
    });
    const subscribed = this.#expect(reply, 'subscribed');
    return { id: subscribed.subscription, snapshot: subscribed.snapshot };
  }

  /** Stops watching a subscription returned by {@link subscribe}. */
  async unsubscribe(subscription: bigint): Promise<void> {
    const reply = await this.#call({
      kind: 'unsubscribe',
      id: this.#nextId++,
      subscription,
    });
    this.#expect(reply, 'done');
  }

  /**
   * Sends one request and returns the response that answers it.
   *
   * There is no pending-request map here: {@link DuetTransport.send} resolves
   * with *this* message's reply, so the transport does the correlating. The
   * `id` still travels because `duet_protocol::decode_request` requires it, and
   * because the webview transport — which has no per-message reply channel at
   * all — correlates by nothing else.
   *
   * The echoed id is still checked. A transport that mis-correlated would
   * otherwise hand this client another request's answer, and every subsequent
   * call would be answered one reply out of step, silently.
   */
  async #call(request: DuetRequest): Promise<DuetResponse> {
    const text = await this.transport.send(encodeRequestText(request));
    // A transport with no host listening resolves with null rather than
    // throwing. Treated as a transport failure, not as silence: left unhandled,
    // the next line would throw a TypeError naming neither the channel nor the
    // request it answered.
    if (text === null) {
      throw new DuetTransportError(
        `no host is listening (null reply to request ${request.id.toString()})`,
      );
    }

    const reply = decodeResponseText(text);
    if (reply.id !== request.id) {
      throw new DuetTransportError(
        `the host answered request ${reply.id.toString()} on the reply to request ` +
          request.id.toString(),
      );
    }
    if (reply.kind === 'failed') {
      throw new DuetFailure(request.id, reply.message);
    }
    return reply;
  }

  /**
   * Narrows a reply to the shape the call site asked for.
   *
   * A mismatch fails here, where the caller can name what it wanted, rather
   * than surfacing later as a confusing undefined somewhere downstream.
   */
  #expect<K extends DuetResponse['kind']>(
    reply: DuetResponse,
    want: K,
  ): Extract<DuetResponse, { kind: K }> {
    if (reply.kind !== want) {
      throw new DuetTransportError(`expected a "${want}" response, got "${reply.kind}"`);
    }
    return reply as Extract<DuetResponse, { kind: K }>;
  }

  /**
   * Handles every unsolicited host-to-guest message.
   *
   * Responses never arrive here — they come back as the reply to
   * {@link DuetTransport.send}.
   *
   * **Total against malformed input by construction.** The decode runs under
   * `try`/`catch (DuetError)`, so no shape of bad data — wrong types, missing
   * fields, a non-object top level, unbounded nesting — can throw out of this
   * method. A push is fire-and-forget from the host's side: there is no request
   * id to fail against, so the only sound response to a malformed push is to
   * drop it.
   *
   * {@link onPush} is deliberately called *outside* the `try`. Swallowing an
   * exception thrown by the application's own handler would hide a bug in code
   * this package does not own, in the one place a developer would never think
   * to look.
   */
  #handleHostMessage(message: string): void {
    let push;
    try {
      push = decodePushText(message);
    } catch (error) {
      if (error instanceof DuetError) return;
      throw error;
    }
    this.onPush?.(push.notification);
  }
}
