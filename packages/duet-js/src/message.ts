/**
 * The Duet message envelope: requests a guest sends, responses the host answers
 * with, and pushes the host originates.
 *
 * Mirrors `duet_protocol::{Request, Response, Push}`
 * (crates/duet-protocol/src/message.rs), their codec
 * (crates/duet-protocol/src/wire.rs), and the Dart port in
 * `packages/duet/lib/src/duet_message.dart`.
 *
 * @module
 */

import { DuetCodecError, DuetReason, echoBounded } from './errors.ts';
import {
  asJsonObject,
  decodeDuetJson,
  encodeDuetJson,
  isCanonicalUnsignedDigits,
  readOwnField,
  readStringField,
  type CanonicalJson,
  type JsonObject,
} from './json.ts';
import { formatDuetPath, parseDuetPath, type DuetPath } from './path.ts';
import {
  decodeOptionalValue,
  decodeValue,
  encodeOptionalValue,
  encodeValue,
  type DuetValue,
} from './value.ts';

/**
 * The inclusive top of the Duet wire's id domain, mirroring
 * `duet_codec::MAX_WIRE_ID` (crates/duet-codec/src/canonical.rs).
 *
 * This is `i64::MAX`, not `u64::MAX`, because of Dart: Dart's native `int` is
 * 64-bit *signed*, so no Dart guest could read a larger id at all. Rather than
 * widen Dart for a range that ids — allocated sequentially from 1 — never
 * reach, the wire domain itself stops here, so one id domain holds in every
 * guest language.
 */
export const MAX_WIRE_ID = 9223372036854775807n;

/**
 * The one channel name every Duet guest and host must agree on.
 *
 * Defined here once, in the transport-agnostic package, so a Flutter binding, a
 * websocket host and a test harness all name the same string by reference
 * rather than retyping a literal. `duet/rpc`, not `duet/protocol`.
 */
export const DUET_CHANNEL_NAME = 'duet/rpc';

/**
 * The most characters a wire id may have: 19 digits for `i64::MAX`.
 *
 * Checked before `BigInt()` runs. See MAX_INT_PAYLOAD_CHARS in `value.ts` for
 * why converting an unbounded digit string from an untrusted peer is a
 * denial-of-service shape rather than a formality.
 */
const MAX_WIRE_ID_CHARS = 19;

/**
 * Parses a wire id, enforcing both halves of the wire's id rule exactly as
 * `duet_codec::parse_wire_id` does. Returns `null` if either half fails.
 *
 * **Canonical spelling** (no leading `+`, no leading zeros): `BigInt('007')` is
 * happy to parse it, which the Rust side rejects. Without this check a
 * JavaScript guest would accept ids a Rust guest refuses, so the two guests
 * would disagree about what is even well-formed — and because the host echoes
 * ids back canonically, a guest that sent `"007"` would be answered `"7"`,
 * never match its own pending entry, and hang with no error at all.
 *
 * **Domain `0..=`{@link MAX_WIRE_ID}**: checked explicitly. `bigint` is
 * unbounded, so nothing else would stop an id past `i64::MAX` — the corpus
 * pins `envelope/id/above_domain` as a rejection for exactly that.
 */
export function tryParseWireId(s: string): bigint | null {
  if (s.length > MAX_WIRE_ID_CHARS) return null;
  if (!isCanonicalUnsignedDigits(s)) return null;
  const parsed = BigInt(s);
  if (parsed < 0n || parsed > MAX_WIRE_ID) return null;
  return parsed;
}

/** Reads the value at `path`. */
export interface DuetGetRequest {
  readonly kind: 'get';
  /** The correlation id. Monotonic per connection. */
  readonly id: bigint;
  /** The path to read. */
  readonly path: DuetPath;
}

/** Writes `value` at `path`. */
export interface DuetSetRequest {
  readonly kind: 'set';
  /** The correlation id. */
  readonly id: bigint;
  /** The path to write. */
  readonly path: DuetPath;
  /** The value to write there. */
  readonly value: DuetValue;
}

/** Starts watching `path`. */
export interface DuetSubscribeRequest {
  readonly kind: 'subscribe';
  /** The correlation id. */
  readonly id: bigint;
  /** The path to watch. */
  readonly path: DuetPath;
}

/**
 * Stops watching a subscription this guest opened.
 *
 * A guest may only cancel its own subscriptions; the host scopes the
 * cancellation to the sender.
 */
export interface DuetUnsubscribeRequest {
  readonly kind: 'unsubscribe';
  /** The correlation id. */
  readonly id: bigint;
  /** The subscription handed back by the matching `subscribed` response. */
  readonly subscription: bigint;
}

/** A guest-to-host message. */
export type DuetRequest =
  | DuetGetRequest
  | DuetSetRequest
  | DuetSubscribeRequest
  | DuetUnsubscribeRequest;

/** The answer to a `get`. */
export interface DuetValueResponse {
  readonly kind: 'value';
  /** The id of the request this answers. */
  readonly id: bigint;
  /**
   * `null` means the path does not exist — distinct from a `null` *value*,
   * which means it exists and holds null.
   */
  readonly value: DuetValue | null;
}

/** The answer to a `set` or an `unsubscribe`: it worked. */
export interface DuetDoneResponse {
  readonly kind: 'done';
  /** The id of the request this answers. */
  readonly id: bigint;
}

/** The answer to a `subscribe`. */
export interface DuetSubscribedResponse {
  readonly kind: 'subscribed';
  /** The id of the request this answers. */
  readonly id: bigint;
  /** The handle to pass to an unsubscribe request to cancel. */
  readonly subscription: bigint;
  /** The watched path's value at subscription time, or `null` if it does not exist. */
  readonly snapshot: DuetValue | null;
}

/** A well-formed rejection of a well-formed request. */
export interface DuetFailedResponse {
  readonly kind: 'failed';
  /** The id of the request this answers. */
  readonly id: bigint;
  /** The host's explanation, safe to show a developer. */
  readonly message: string;
}

/** A host-to-guest reply to exactly one {@link DuetRequest}. */
export type DuetResponse =
  | DuetValueResponse
  | DuetDoneResponse
  | DuetSubscribedResponse
  | DuetFailedResponse;

/**
 * One change to a watched path.
 *
 * Mirrors `duet_core::Notification`, with its `Patch` flattened: the wire nests
 * `{path, value}` under a `patch` object, but a guest only ever wants the two
 * fields, so the nesting stays in the codec rather than becoming a second type
 * for callers to unwrap.
 */
export interface DuetNotification {
  /** The host's own `SubscriberId`. Informational: a guest never names one. */
  readonly subscriber: bigint;
  /** Which of this guest's `subscribe` calls this notification answers. */
  readonly subscription: bigint;
  /** The path that changed. */
  readonly path: DuetPath;
  /** The path's new value. */
  readonly value: DuetValue;
}

/** A watched path changed. */
export interface DuetNotificationPush {
  readonly kind: 'notification';
  /** What changed, and for whom. */
  readonly notification: DuetNotification;
}

/** An unsolicited host-to-guest message, answering no request. */
export type DuetPush = DuetNotificationPush;

/** Encodes a request to its wire form. */
export function encodeRequest(request: DuetRequest): CanonicalJson {
  switch (request.kind) {
    case 'get':
      return fields([
        ['kind', 'get'],
        ['id', request.id.toString()],
        ['path', formatDuetPath(request.path)],
      ]);
    case 'set':
      return fields([
        ['kind', 'set'],
        ['id', request.id.toString()],
        ['path', formatDuetPath(request.path)],
        ['value', encodeValue(request.value)],
      ]);
    case 'subscribe':
      return fields([
        ['kind', 'subscribe'],
        ['id', request.id.toString()],
        ['path', formatDuetPath(request.path)],
      ]);
    case 'unsubscribe':
      return fields([
        ['kind', 'unsubscribe'],
        ['id', request.id.toString()],
        ['subscription', request.subscription.toString()],
      ]);
  }
}

/** Encodes a request as the exact text the wire carries. */
export function encodeRequestText(request: DuetRequest): string {
  return encodeDuetJson(encodeRequest(request));
}

/**
 * Decodes a request from wire text.
 *
 * Throws {@link DuetCodecError} and nothing else, whatever `text` contains.
 */
export function decodeRequestText(text: string): DuetRequest {
  return decodeRequest(decodeDuetJson(text));
}

/**
 * Decodes a request from already-parsed JSON.
 *
 * The `id` is read before the `kind` is dispatched on, matching
 * `duet_protocol::decode_request`: a message with both a bad id and an unknown
 * kind is reported as a bad id, in every language.
 */
export function decodeRequest(json: unknown): DuetRequest {
  const obj = asJsonObject(json, 'request');
  const id = idField(obj, 'id');
  const kind = readStringField(obj, 'kind');
  switch (kind) {
    case 'get':
      return { kind: 'get', id, path: pathField(obj) };
    case 'set':
      return { kind: 'set', id, path: pathField(obj), value: decodeValue(readOwnField(obj, 'value')) };
    case 'subscribe':
      return { kind: 'subscribe', id, path: pathField(obj) };
    case 'unsubscribe':
      return { kind: 'unsubscribe', id, subscription: idField(obj, 'subscription') };
    default:
      throw new DuetCodecError(
        DuetReason.unknownTag,
        `unknown request kind ${echoBounded(kind)}`,
      );
  }
}

/** Encodes a response to its wire form. */
export function encodeResponse(response: DuetResponse): CanonicalJson {
  switch (response.kind) {
    case 'value':
      return fields([
        ['kind', 'value'],
        ['id', response.id.toString()],
        ['value', encodeOptionalValue(response.value)],
      ]);
    case 'done':
      return fields([
        ['kind', 'done'],
        ['id', response.id.toString()],
      ]);
    case 'subscribed':
      return fields([
        ['kind', 'subscribed'],
        ['id', response.id.toString()],
        ['subscription', response.subscription.toString()],
        ['snapshot', encodeOptionalValue(response.snapshot)],
      ]);
    case 'failed':
      return fields([
        ['kind', 'failed'],
        ['id', response.id.toString()],
        ['message', response.message],
      ]);
  }
}

/** Encodes a response as the exact text the wire carries. */
export function encodeResponseText(response: DuetResponse): string {
  return encodeDuetJson(encodeResponse(response));
}

/**
 * Decodes a response from wire text.
 *
 * Throws {@link DuetCodecError} and nothing else, whatever `text` contains.
 */
export function decodeResponseText(text: string): DuetResponse {
  return decodeResponse(decodeDuetJson(text));
}

/** Decodes a response from already-parsed JSON. */
export function decodeResponse(json: unknown): DuetResponse {
  const obj = asJsonObject(json, 'response');
  const id = idField(obj, 'id');
  const kind = readStringField(obj, 'kind');
  switch (kind) {
    case 'value':
      return { kind: 'value', id, value: decodeOptionalValue(readOwnField(obj, 'value')) };
    case 'done':
      return { kind: 'done', id };
    case 'subscribed':
      return {
        kind: 'subscribed',
        id,
        subscription: idField(obj, 'subscription'),
        snapshot: decodeOptionalValue(readOwnField(obj, 'snapshot')),
      };
    case 'failed':
      return { kind: 'failed', id, message: readStringField(obj, 'message') };
    default:
      throw new DuetCodecError(
        DuetReason.unknownTag,
        `unknown response kind ${echoBounded(kind)}`,
      );
  }
}

/** Encodes a push to its wire form. */
export function encodePush(push: DuetPush): CanonicalJson {
  return fields([
    ['kind', 'notification'],
    ['notification', encodeNotification(push.notification)],
  ]);
}

/** Encodes a push as the exact text the wire carries. */
export function encodePushText(push: DuetPush): string {
  return encodeDuetJson(encodePush(push));
}

/**
 * Decodes a push from wire text.
 *
 * Throws {@link DuetCodecError} and nothing else, whatever `text` contains.
 */
export function decodePushText(text: string): DuetPush {
  return decodePush(decodeDuetJson(text));
}

/** Decodes a push from already-parsed JSON. */
export function decodePush(json: unknown): DuetPush {
  const obj = asJsonObject(json, 'push');
  const kind = readStringField(obj, 'kind');
  if (kind !== 'notification') {
    throw new DuetCodecError(DuetReason.unknownTag, `unknown push kind ${echoBounded(kind)}`);
  }
  return { kind: 'notification', notification: decodeNotification(readOwnField(obj, 'notification')) };
}

/** Encodes a notification to its wire form, `patch` nesting restored. */
export function encodeNotification(note: DuetNotification): CanonicalJson {
  return fields([
    ['subscriber', note.subscriber.toString()],
    ['subscription', note.subscription.toString()],
    [
      'patch',
      fields([
        ['path', formatDuetPath(note.path)],
        ['value', encodeValue(note.value)],
      ]),
    ],
  ]);
}

/** Decodes a notification from already-parsed JSON. */
export function decodeNotification(json: unknown): DuetNotification {
  const obj = asJsonObject(json, 'notification');
  const patch = asJsonObject(readOwnField(obj, 'patch'), 'patch');
  return {
    subscriber: idField(obj, 'subscriber'),
    subscription: idField(obj, 'subscription'),
    path: parseDuetPath(readStringField(patch, 'path')),
    value: decodeValue(readOwnField(patch, 'value')),
  };
}

/**
 * Builds an envelope object.
 *
 * Field order here is *declaration* order and is deliberately not the wire's:
 * {@link encodeDuetJson} sorts every object's keys as it writes the text, so
 * this list is free to read in the order a human would write it.
 */
function fields(entries: readonly (readonly [string, CanonicalJson])[]): CanonicalJson {
  return new Map<string, CanonicalJson>(entries);
}

/** Reads a required path field. */
function pathField(obj: JsonObject): DuetPath {
  return parseDuetPath(readStringField(obj, 'path'));
}

/**
 * Reads one of the envelope's id fields through the single definition of the
 * wire's id rule, {@link tryParseWireId}.
 *
 * Gates every id the envelope carries: `id` on requests and responses,
 * `subscription` on `unsubscribe` and `subscribed`, and both `subscriber` and
 * `subscription` on a notification.
 */
function idField(obj: JsonObject, name: string): bigint {
  const raw = readOwnField(obj, name);
  if (typeof raw !== 'string') {
    throw new DuetCodecError(DuetReason.badShape, `"${name}" must be a decimal string`);
  }
  const parsed = tryParseWireId(raw);
  if (parsed === null) {
    throw new DuetCodecError(
      DuetReason.badInt,
      `"${name}" is not a canonical decimal string in 0..${MAX_WIRE_ID.toString()}: ` +
        echoBounded(raw),
    );
  }
  return parsed;
}
