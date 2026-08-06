/**
 * The JSON layer underneath the Duet wire format: parsing untrusted text,
 * emitting canonical text, and the canonical order of object keys.
 *
 * Mirrors `packages/duet/lib/src/duet_json.dart`, with one deliberate
 * difference documented on {@link encodeDuetJson}: the canonical encoder here
 * writes the JSON text itself instead of handing an object to
 * `JSON.stringify`, because `JSON.stringify` cannot be made to emit
 * integer-like object keys in canonical order.
 *
 * @module
 */

import { DuetCodecError, DuetReason, MAX_ECHO_CHARS, MAX_JSON_DEPTH } from './errors.ts';

/**
 * A JSON document in the shape this package emits.
 *
 * Objects are `Map`s rather than plain objects. That is the whole point of the
 * type: a `Map` preserves insertion order for *every* key, whereas a plain
 * object reorders integer-like keys (`"0"`, `"42"`) ahead of the rest, and no
 * sort can override it. See {@link encodeDuetJson}.
 */
export type CanonicalJson =
  | null
  | boolean
  | number
  | string
  | readonly CanonicalJson[]
  | ReadonlyMap<string, CanonicalJson>;

/**
 * Parses one Duet wire message, refusing anything the Rust peer's JSON parser
 * would refuse.
 *
 * Two failure modes, both reported as {@link DuetReason.badJson} so the three
 * implementations agree on the reason as well as the verdict:
 *
 * - the text is not JSON, which `JSON.parse` reports as a `SyntaxError`;
 * - the text nests containers deeper than {@link MAX_JSON_DEPTH}, which
 *   `JSON.parse` accepts and `serde_json` does not.
 *
 * The depth check is the reason this function exists rather than a bare
 * `JSON.parse` call. It must run *before* any of this package's decoders touch
 * the tree, because those decoders recurse: an unbounded tree would overflow
 * the stack on the way down. V8's `JSON.parse` accepted a 100 000-deep
 * document in this project's own measurement, which is exactly why it cannot
 * be relied on to raise the alarm.
 *
 * Throws {@link DuetCodecError} and nothing else, whatever `text` contains.
 *
 * @param text untrusted wire text
 * @returns the parsed JSON tree, still untyped — the layer above narrows it
 */
export function decodeDuetJson(text: string): unknown {
  let parsed: unknown;
  try {
    parsed = JSON.parse(text) as unknown;
  } catch (cause) {
    // Deliberately catches everything, not just `SyntaxError`. V8 throws a
    // `RangeError` ("Maximum call stack size exceeded") for input deep enough
    // to exhaust its own parser, and a decoder that let that escape would be
    // handing its caller a failure mode outside this package's error type.
    throw new DuetCodecError(DuetReason.badJson, `malformed JSON: ${describeCause(cause)}`);
  }
  requireBoundedDepth(parsed);
  return parsed;
}

/**
 * Serializes `doc` to the compact text the wire carries, with every object's
 * keys in canonical order.
 *
 * # Why this is hand-written rather than `JSON.stringify`
 *
 * The Rust host serializes through `serde_json::Map`, which is a `BTreeMap` in
 * this workspace, so its object keys come out in byte order —
 * `{"id":"7","kind":"get","path":"a"}`, not the order the fields are declared
 * in. Two separate JavaScript problems stand between `JSON.stringify` and that
 * output:
 *
 * 1. `JSON.stringify` walks a plain object in *property* order, so a client
 *    that built `{kind, id, path}` would emit declaration order and fail every
 *    byte-exact case in the golden corpus. Sorting the keys into a fresh
 *    object fixes this much.
 * 2. It does not fix the rest. ECMAScript's own property ordering puts
 *    **integer-like keys** (`"0"`, `"1"`, `"42"`) first, in ascending numeric
 *    order, whatever order they were inserted in — `JSON.stringify({'0':1,
 *    '!':2})` is `{"0":1,"!":2}` where canonical order is `"!"` then `"0"`,
 *    and `{'9':1,'10':2}` comes out `"9"` before `"10"` where canonical order
 *    is the reverse. No comparator can override that, because no comparator
 *    ever runs.
 *
 * So a Duet map whose keys look like array indices cannot be emitted in
 * canonical order from a plain JavaScript object at all. Writing the text
 * directly from a `Map` — which preserves insertion order for every key — is
 * the only fix, and it is why {@link CanonicalJson} is built on `Map`.
 * (`crates/duet-webview/src/bootstrap.rs` documents this as a caveat its
 * guests must live with; this package does not have to.)
 *
 * Strings still go through `JSON.stringify`, which already agrees byte for
 * byte with `serde_json`'s escaper: short escapes for `\b \t \n \f \r`,
 * lowercase `\u00xx` for the other control characters, and no escaping of `/`
 * or of non-ASCII.
 *
 * @param doc the document to write
 * @returns compact JSON text
 * @throws DuetCodecError if the document nests past {@link MAX_JSON_DEPTH}, or
 *   holds a number no JSON text can denote
 */
export function encodeDuetJson(doc: CanonicalJson): string {
  const out: string[] = [];
  writeJson(doc, 1, out);
  return out.join('');
}

/**
 * Orders two JSON object keys by **code point** — the Duet wire's canonical
 * order, and equivalently UTF-8 byte order (UTF-8 is constructed so that
 * byte-wise comparison of encoded strings agrees with code-point comparison).
 * This is the order Rust's `BTreeMap<String, _>` produces for free, which is
 * why `duet-codec` contains no sorting code at all.
 *
 * # Do NOT replace this with `a < b`, `a.localeCompare(b)` or a bare `.sort()`
 *
 * A future maintainer will look at this and see a hand-rolled version of the
 * built-in. It is not. JavaScript's relational operators and the default
 * `Array.prototype.sort` compare **UTF-16 code units**, and those disagree
 * with code points above the BMP:
 *
 * - `U+1F600` (😀) is encoded in UTF-16 as the surrogate pair `D83D DE00`.
 * - `0xD83D` is numerically **less than** `0xE000`.
 * - So the built-ins sort `U+1F600` *before* `U+E000`, while code-point
 *   order — and Rust — sorts it *after*.
 *
 * `localeCompare` is worse still: it is locale-sensitive, so the same input
 * can order differently on two machines.
 *
 * Every key below `U+D800` compares identically under all of these rules, so a
 * test using ASCII or Latin-1 keys passes either way. That is precisely how
 * this divergence went unnoticed; the corpus case `value/map/code_point_order`
 * uses `U+E000`, `U+FFFD` and `U+1F600` deliberately.
 *
 * The `for...of` loop is what makes this correct: iterating a string yields
 * code points, pairing surrogates back up, whereas indexing one yields code
 * units.
 */
export function compareDuetMapKeys(a: string, b: string): number {
  const left = a[Symbol.iterator]();
  const right = b[Symbol.iterator]();
  for (;;) {
    const x = left.next();
    const y = right.next();
    // One ran out: the shorter string is a prefix of the longer, so it sorts
    // first. Both ran out together means the strings are equal.
    if (x.done === true) return y.done === true ? 0 : -1;
    if (y.done === true) return 1;
    const xc = x.value.codePointAt(0) ?? 0;
    const yc = y.value.codePointAt(0) ?? 0;
    if (xc !== yc) return xc < yc ? -1 : 1;
  }
}

/**
 * True if `s` is a canonical, sign-free decimal digit string: at least one
 * digit, and no leading zero unless `s` is exactly `"0"`.
 *
 * Mirrors `duet_codec::is_canonical_unsigned_digits`
 * (crates/duet-codec/src/canonical.rs). This is the rule for every
 * **unsigned** integer on the wire — the `id`, `subscription` and `subscriber`
 * fields.
 *
 * It matters because those ids are *echoed* by the host in canonical form. A
 * guest that accepted `"007"`, keyed a pending entry by the string it sent,
 * and then received `"7"` back would never match the two — a silent hang with
 * no error at all. `BigInt("007")` is happy to parse it, and `Number("+1")` is
 * happy to parse that, so this predicate has to gate them.
 *
 * Validates *spelling only*: `"18446744073709551615"` is spelled canonically
 * and is still outside the wire's id domain. See `tryParseWireId` in
 * `message.ts` for the paired range check.
 */
export function isCanonicalUnsignedDigits(s: string): boolean {
  if (s.length === 0) return false;
  for (let i = 0; i < s.length; i++) {
    const c = s.charCodeAt(i);
    if (c < 0x30 || c > 0x39) return false;
  }
  return s === '0' || !s.startsWith('0');
}

/**
 * True if `s` is a canonical **signed** decimal string: an optional `-`
 * followed by canonical digits, and never `"-0"`.
 *
 * Mirrors `duet_codec::is_canonical_signed_decimal`. This is the rule for
 * `Value::Int` payloads, which — unlike ids — may be negative.
 */
export function isCanonicalSignedDecimal(s: string): boolean {
  if (s.startsWith('-')) {
    const magnitude = s.slice(1);
    return magnitude !== '0' && isCanonicalUnsignedDigits(magnitude);
  }
  return isCanonicalUnsignedDigits(s);
}

/**
 * Names a decoded JSON value's type without rendering its contents.
 *
 * Mirrors `duet_codec`'s `json_type_name`. Rendering an arbitrary
 * guest-supplied tree into an error message would be an O(n) walk and
 * allocation on the error path for whatever the peer sent — a denial-of-service
 * shape on a hot IPC path. Naming the type is O(1) and still actionable.
 */
export function jsonTypeName(json: unknown): string {
  if (json === null) return 'null';
  switch (typeof json) {
    case 'boolean':
      return 'a boolean';
    case 'number':
      return 'a number';
    case 'string':
      return 'a string';
    default:
      return Array.isArray(json) ? 'an array' : 'an object';
  }
}

/**
 * A JSON object as `JSON.parse` produces one: own string keys, unknown values.
 *
 * Not `Record<string, unknown>` by accident — every read of one of these goes
 * through {@link readOwnField}, never through plain property access, so a key
 * inherited from `Object.prototype` can never be mistaken for a field the peer
 * actually sent.
 */
export type JsonObject = { readonly [key: string]: unknown };

/**
 * Narrows `json` to an object, or refuses it by name with
 * {@link DuetReason.badShape}.
 *
 * Arrays are objects in JavaScript, so the `Array.isArray` arm is load-bearing:
 * without it `["t"]` would pass as an object and then fail later with a
 * confusing "missing t" instead of "expected an object, found an array".
 */
export function asJsonObject(json: unknown, what: string): JsonObject {
  if (json === null || typeof json !== 'object' || Array.isArray(json)) {
    throw new DuetCodecError(
      DuetReason.badShape,
      `${what} must be an object, found ${jsonTypeName(json)}`,
    );
  }
  return json as JsonObject;
}

/**
 * True if `obj` carries `name` as its **own** property.
 *
 * `name in obj` and `obj[name] !== undefined` are both wrong here: the first
 * finds inherited members, so `"constructor" in {}` is true, and the second
 * cannot tell an absent field from one explicitly sent as `null` — a
 * distinction the wire draws (see `decodeOptionalValue`).
 */
export function hasOwnField(obj: JsonObject, name: string): boolean {
  return Object.hasOwn(obj, name);
}

/**
 * Reads a required own field, refusing a missing one with
 * {@link DuetReason.badShape}.
 */
export function readOwnField(obj: JsonObject, name: string): unknown {
  if (!hasOwnField(obj, name)) {
    throw new DuetCodecError(DuetReason.badShape, `missing "${name}"`);
  }
  return obj[name];
}

/** Reads a required own field that must be a JSON string. */
export function readStringField(obj: JsonObject, name: string): string {
  const raw = readOwnField(obj, name);
  if (typeof raw !== 'string') {
    throw new DuetCodecError(DuetReason.badShape, `"${name}" must be a string`);
  }
  return raw;
}

/**
 * Writes one node of a {@link CanonicalJson} document into `out`.
 *
 * `depth` counts the containers enclosing this node, including it. The bound
 * is the encoder's half of {@link MAX_JSON_DEPTH}: a document deeper than the
 * wire admits would be refused by every peer anyway, and emitting it would
 * mean recursing far enough to overflow the stack first. Failing loudly here
 * turns an uncatchable crash into a diagnosable error.
 */
function writeJson(node: CanonicalJson, depth: number, out: string[]): void {
  if (node === null) {
    out.push('null');
    return;
  }
  switch (typeof node) {
    case 'boolean':
      out.push(node ? 'true' : 'false');
      return;
    case 'string':
      out.push(JSON.stringify(node));
      return;
    case 'number':
      out.push(writeNumber(node));
      return;
    default:
      break;
  }

  if (depth > MAX_JSON_DEPTH) {
    throw new DuetCodecError(
      DuetReason.badJson,
      `document nests deeper than ${MAX_JSON_DEPTH} JSON containers`,
    );
  }

  if (Array.isArray(node)) {
    out.push('[');
    for (let i = 0; i < node.length; i++) {
      if (i > 0) out.push(',');
      writeJson(node[i] as CanonicalJson, depth + 1, out);
    }
    out.push(']');
    return;
  }

  // Sorting here, at the one place text is produced, rather than at each call
  // site that builds a document: a field added to an envelope later cannot
  // reintroduce the bug by being appended in the wrong place, because no call
  // site is trusted to have ordered anything.
  const fields = [...(node as ReadonlyMap<string, CanonicalJson>).entries()].sort((a, b) =>
    compareDuetMapKeys(a[0], b[0]),
  );
  out.push('{');
  for (let i = 0; i < fields.length; i++) {
    if (i > 0) out.push(',');
    out.push(JSON.stringify(fields[i]![0]), ':');
    writeJson(fields[i]![1]!, depth + 1, out);
  }
  out.push('}');
}

/**
 * Renders a JSON number, refusing the two spellings JavaScript cannot express.
 *
 * `NaN` and the infinities have no JSON literal, and `JSON.stringify` silently
 * turns all three into `null`. Negative zero is subtler: `JSON.stringify(-0)`
 * is `"0"`, so the sign vanishes without a word. All four travel as string
 * sentinels on this wire (see `encodeFloat` in `value.ts`), so reaching this
 * function with one of them means a caller hand-built a document and skipped
 * the value encoder — a bug worth a loud error rather than a wrong wire byte.
 *
 * `Object.is(n, -0)` is the only correct negative-zero test: `n === -0` is also
 * true for `+0` and would reject every zero.
 */
function writeNumber(n: number): string {
  if (!Number.isFinite(n) || Object.is(n, -0)) {
    throw new DuetCodecError(
      DuetReason.badFloat,
      `${String(n)} has no JSON number spelling; it must travel as a string sentinel`,
    );
  }
  return JSON.stringify(n) as string;
}

/**
 * Walks `root` iteratively, throwing if any container nests past
 * {@link MAX_JSON_DEPTH}.
 *
 * Iterative on purpose: a recursive check would overflow the stack on exactly
 * the input it exists to reject, since `JSON.parse` will have already built the
 * pathological tree without complaint. The explicit stack is bounded by the
 * number of nodes in the parsed document, which the caller has already paid
 * for; it allocates nothing proportional to anything else.
 */
function requireBoundedDepth(root: unknown): void {
  // Each entry is a node paired with the number of containers enclosing it.
  const pending: { node: unknown; depth: number }[] = [{ node: root, depth: 0 }];
  while (pending.length > 0) {
    const entry = pending.pop() as { node: unknown; depth: number };
    const node = entry.node;
    if (node === null || typeof node !== 'object') continue;
    const inner = entry.depth + 1;
    if (inner > MAX_JSON_DEPTH) {
      throw new DuetCodecError(
        DuetReason.badJson,
        `JSON nests deeper than ${MAX_JSON_DEPTH} containers`,
      );
    }
    if (Array.isArray(node)) {
      for (const child of node) pending.push({ node: child, depth: inner });
    } else {
      const obj = node as JsonObject;
      for (const key of Object.keys(obj)) pending.push({ node: obj[key], depth: inner });
    }
  }
}

/**
 * Renders whatever `JSON.parse` threw, bounded.
 *
 * V8's parse errors quote the offending input ("Unexpected token } in JSON at
 * position 5"), so the message is peer-controlled text and gets the same length
 * bound as everything else that crosses this boundary.
 */
function describeCause(cause: unknown): string {
  const text = cause instanceof Error ? cause.message : String(cause);
  return text.length <= MAX_ECHO_CHARS ? text : `${text.slice(0, MAX_ECHO_CHARS)}…`;
}
