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

/**
 * Runs a host command.
 *
 * Deliberately carries **no** caller identity — no subscriber, no surface — for
 * the same reason {@link DuetSubscribeRequest} carries no subscriber. Which
 * commands a guest may reach is decided by the `CommandHost` its surface was
 * built with (crates/duet-protocol/src/command.rs), never by the guest: a guest
 * that could name its own caller identity could name another guest's, and
 * authorization decided by the party being authorized is not authorization.
 */
export interface DuetInvokeRequest {
  readonly kind: 'invoke';
  /** The correlation id. */
  readonly id: bigint;
  /** Which command to run. */
  readonly command: string;
  /**
   * The command's named arguments.
   *
   * A `Map` and not a {@link DuetValue}, mirroring `duet_protocol::Args`: a
   * command's parameter list is a struct's field list, so a non-map `args` is an
   * illegal state this type does not admit. The *wire* still carries a tagged
   * map, so both ends reuse the value codec they already have rather than
   * growing a second, arguments-only one that could disagree with the first.
   *
   * A `Map` rather than a plain object for the same two reasons
   * {@link DuetMap} is one: key order survives for integer-like keys, and a key
   * named `__proto__` is just a key.
   */
  readonly args: ReadonlyMap<string, DuetValue>;
}

/** A guest-to-host message. */
export type DuetRequest =
  | DuetGetRequest
  | DuetSetRequest
  | DuetSubscribeRequest
  | DuetUnsubscribeRequest
  | DuetInvokeRequest;

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

/**
 * A command ran and returned.
 *
 * Always carries a tagged value; a command with no result answers a value of
 * kind `'null'`, spelled `{"t":"n"}`. There is no absent case here, which is why
 * `value` is not nullable: JSON `null` already means *"the path is absent"*
 * everywhere else in this format — see {@link DuetValueResponse} — and spending
 * that spelling twice, on two different questions, is how a format ends up with
 * a value nobody can interpret without knowing which field they are looking at.
 */
export interface DuetReturnedResponse {
  readonly kind: 'returned';
  /** The id of the request this answers. */
  readonly id: bigint;
  /** What the command returned. */
  readonly value: DuetValue;
}

/**
 * A command ran and returned an error — a **domain** outcome, not a refusal.
 *
 * Distinct from {@link DuetFailedResponse} in two ways that both matter to a
 * caller. `failed` carries a `string`, and flattening a developer's typed error
 * into prose is not reversible: a guest that wanted to match on
 * `InsufficientFunds { shortBy }` would get a sentence to regex instead. And the
 * two are different *events* — `failed` says the call did not happen, `raised`
 * says it happened and the answer was a failure — so a guest that could not tell
 * them apart could not decide whether retrying is safe.
 */
export interface DuetRaisedResponse {
  readonly kind: 'raised';
  /** The id of the request this answers. */
  readonly id: bigint;
  /** The error the command returned, tagged like any other value. */
  readonly error: DuetValue;
}

/** A host-to-guest reply to exactly one {@link DuetRequest}. */
export type DuetResponse =
  | DuetValueResponse
  | DuetDoneResponse
  | DuetSubscribedResponse
  | DuetFailedResponse
  | DuetReturnedResponse
  | DuetRaisedResponse;

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
    case 'invoke':
      return fields([
        ['kind', 'invoke'],
        ['id', request.id.toString()],
        ['command', request.command],
        // Through the ordinary tagged-value path, as a map, rather than as a
        // bare JSON object of tagged values. One encoding for values, used
        // everywhere, is what lets the host reuse the decoder it already has
        // instead of growing a second, arguments-only one that could disagree
        // with the first.
        ['args', encodeValue({ kind: 'map', entries: request.args })],
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
    case 'invoke':
      return {
        kind: 'invoke',
        id,
        command: readStringField(obj, 'command'),
        args: argsField(obj),
      };
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
    case 'returned':
      // `encodeValue`, never `encodeOptionalValue`: this field has no absent
      // case, and a unit return is `{"t":"n"}`.
      return fields([
        ['kind', 'returned'],
        ['id', response.id.toString()],
        ['value', encodeValue(response.value)],
      ]);
    case 'raised':
      return fields([
        ['kind', 'raised'],
        ['id', response.id.toString()],
        ['error', encodeValue(response.error)],
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
    case 'returned':
      return { kind: 'returned', id, value: requiredValueField(obj, 'value') };
    case 'raised':
      return { kind: 'raised', id, error: requiredValueField(obj, 'error') };
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
 * Reads an `args` field: a tagged value that must be a map.
 *
 * Two steps, in this order, and the order is the point. The tagged value is
 * decoded first, by the decoder that is already total against hostile input;
 * only then is it narrowed to a map. Narrowing first would mean writing a second
 * decoder for the same bytes. Mirrors `args_field`
 * (crates/duet-protocol/src/wire.rs).
 */
function argsField(obj: JsonObject): ReadonlyMap<string, DuetValue> {
  const decoded = decodeValue(readOwnField(obj, 'args'));
  if (decoded.kind === 'map') return decoded.entries;
  throw new DuetCodecError(
    DuetReason.badShape,
    `"args" must be a tagged map, got a tagged ${valueKindName(decoded)}`,
  );
}

/**
 * Names a value's kind for an error message.
 *
 * The kind alone, never anything derived from the value: the value is
 * peer-supplied, so rendering *it* would be the unbounded echo this package's
 * errors already bound against.
 *
 * `'str'` is reported as `string`, matching `value_kind`
 * (crates/duet-protocol/src/wire.rs) — the wire's own vocabulary, so one refusal
 * reads the same whichever implementation produced it.
 */
function valueKindName(value: DuetValue): string {
  return value.kind === 'str' ? 'string' : value.kind;
}

/**
 * Reads a field that must carry a tagged value, refusing JSON `null`.
 *
 * The counterpart to {@link decodeOptionalValue}, and the reason both exist.
 * This format already spends JSON `null` on one meaning — *"the path is
 * absent"* — and a `returned` or `raised` has no absent case to express: a
 * command that returns nothing returns a value of kind `'null'`, which is
 * `{"t":"n"}`.
 *
 * Accepting bare `null` here would make `{"t":"n"}` and `null` two spellings of
 * one thing on one field and two different things on another, which is exactly
 * the kind of context-dependent rule three independent decoders cannot be
 * expected to keep straight. So it is refused, and the refusal names what to
 * send instead — kept short so {@link echoBounded}'s cap cannot cut the
 * `{"t":"n"}` hint, which is the only actionable part. Mirrors `required_value`
 * (crates/duet-protocol/src/wire.rs).
 */
function requiredValueField(obj: JsonObject, name: string): DuetValue {
  const raw = readOwnField(obj, name);
  if (raw === null) {
    throw new DuetCodecError(
      DuetReason.badShape,
      `"${name}" must be tagged; null is {"t":"n"}`,
    );
  }
  return decodeValue(raw);
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
