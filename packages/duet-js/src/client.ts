/**
 * The guest client: `window.__duet`, typed.
 *
 * Mirrors `packages/duet/lib/src/duet_client.dart`.
 *
 * @module
 */

import { DuetError, DuetFailure, DuetTransportError, echoBounded } from './errors.ts';
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

/** The command ran and returned `Ok`. */
export interface DuetReturned {
  readonly kind: 'returned';
  /** What the command returned. A command with no result answers kind `'null'`. */
  readonly value: DuetValue;
}

/**
 * The command ran and returned `Err` — a **domain** outcome.
 *
 * Carries the error as a value, not as prose, so an application can decode it
 * back into its own error type. That is the entire reason this is not a
 * {@link DuetFailure}: flattening a typed error into a sentence is not
 * reversible.
 */
export interface DuetRaised {
  readonly kind: 'raised';
  /** The error the command returned, tagged like any other value. */
  readonly error: DuetValue;
}

/**
 * What {@link DuetClient.invoke} hands back: the two ways a command that **ran**
 * can end.
 *
 * # Why this is a result and not two exceptions
 *
 * The protocol draws its line between *the command ran* and *the host would not
 * run it*, and this type draws the same one. `returned` and `raised` are both
 * answers from a body that executed, so they are **data**; `failed` — no such
 * command, arguments that did not decode, a body that panicked — means the call
 * never happened, so it stays a thrown {@link DuetFailure} alongside every other
 * host refusal this client throws.
 *
 * This is the `DuetReading` argument applied to a narrower question.
 * `DuetReading` has four arms because the host has four states a path can be in
 * and the wire spends a distinct spelling on each; collapsing any two would
 * delete a distinction the protocol pays for. An `invoke` that ran has exactly
 * two, and the wire spends `returned` and `raised` on them. Where the two types
 * differ is in what they do with *failure*: a `DuetReading` is also produced by a
 * push, which has no call stack to throw into, so `get` may not throw either. An
 * `invoke` is only ever a reply to a call this guest made, so there is a stack,
 * and a refusal is free to use it.
 *
 * # What a caller must do with it
 *
 * `switch` over `kind`. TypeScript narrows a discriminated union exhaustively, so
 * a `switch` that handles only {@link DuetReturned} and returns a non-`undefined`
 * type fails to compile — which is the failure this type exists to make
 * impossible, and which an `invoke` returning a bare {@link DuetValue} would
 * invite.
 *
 * # Where this diverges from the Dart mirror, and why
 *
 * Dart's version is a `sealed` class hierarchy; TypeScript's unions are
 * structural, so this is a union of two plain objects. Same arms, same tags,
 * same semantics — the identical divergence `DuetReading` already documents.
 */
export type DuetInvocation = DuetReturned | DuetRaised;

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
   * Runs the host command named `command` with `args`.
   *
   * `args` is a `Map` because a command's parameter list is a struct's field
   * list; it is encoded as a tagged map with its keys sorted, so the request
   * bytes are canonical whatever order the caller built it in. A command that
   * takes nothing is called with no second argument.
   *
   * @returns {@link DuetReturned} or {@link DuetRaised} for a command that
   *   **ran**. See {@link DuetInvocation} for why a refusal is not an arm here.
   * @throws DuetFailure if the host **refused** to run it: there is no command
   *   by that name for this surface, the arguments did not decode, or the body
   *   panicked.
   * @throws DuetTransportError if the exchange never reached the protocol, or if
   *   the host answered something that is neither a `returned` nor a `raised`.
   */
  async invoke(
    command: string,
    args: ReadonlyMap<string, DuetValue> = new Map<string, DuetValue>(),
  ): Promise<DuetInvocation> {
    const reply = await this.#call({ kind: 'invoke', id: this.#nextId++, command, args });
    switch (reply.kind) {
      case 'returned':
        return { kind: 'returned', value: reply.value };
      case 'raised':
        return { kind: 'raised', error: reply.error };
      default:
        // A `failed` never reaches here — `#call` has already thrown it — so
        // this arm is a host that answered an `invoke` with a `done` or a
        // `value`. That is a protocol violation rather than an outcome, and it
        // gets the same treatment `#expect` gives every other mismatched reply.
        throw new DuetTransportError(
          `expected a "returned" or "raised" response to invoke ` +
            `${echoBounded(command)}, got "${reply.kind}"`,
        );
    }
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
      // A `failed` naming an id this call did not send is the host saying it
      // could not read the id of the request it is refusing — see
      // `RequestId::UNCORRELATED` in crates/duet-protocol/src/message.rs. Its
      // `message` is the only account of *why*, so it travels with the error.
      // Reporting the id mismatch alone would replace a diagnosis ("malformed
      // JSON: lone surrogate at line 1 column 34") with a correlation
      // complaint, which is exactly the wrong half of the story.
      const detail =
        reply.kind === 'failed'
          ? `, and refused it: ${reply.message}`
          : '';
      throw new DuetTransportError(
        `the host answered request ${reply.id.toString()} on the reply to request ` +
          request.id.toString() +
          detail,
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
