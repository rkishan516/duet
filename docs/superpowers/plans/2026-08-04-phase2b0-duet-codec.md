# Phase 2b-0 — `duet-codec` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A lossless, round-trip-exact wire format for everything that crosses the IPC boundary between the Rust host and its Flutter and JavaScript guests.

**Architecture:** A **tagged** JSON encoding — every `Value` becomes `{"t":"<tag>","v":<payload>}` — because JSON's type system cannot represent `Value` faithfully otherwise. The API is free functions rather than a `Codec` trait: spec §6.3 anticipates a trait so a binary codec can drop in, but with exactly one implementation a trait would be speculative, and callers depending on functions rather than a concrete type preserves the same freedom. `duet-core` stays zero-dependency: conversion is written by hand rather than by deriving `Serialize` on core's types.

**Tech Stack:** Rust 1.92, edition 2024. One external dependency, `serde_json`. Base64 is hand-rolled (40 lines, total, unambiguous) rather than adding a second.

**Reference:** `docs/superpowers/specs/2026-08-04-duet-design.md` §6.3 (transport).

---

## Background for the implementer

### What crosses the wire

`duet-core` (zero deps) defines the types. You will encode and decode:

```rust
enum Value { Null, Bool(bool), Int(i64), Float(f64), Str(String),
             Bytes(Vec<u8>), List(Vec<Value>), Map(BTreeMap<String, Value>) }

struct Path(Vec<Segment>);        // Segment = Key(String) | Index(usize)
struct Patch { path: Path, value: Value }
struct Notification { subscriber: SubscriberId, subscription: SubscriptionId, patch: Patch }
```

`SubscriberId(pub u64)` and `SubscriptionId(pub u64)` are newtypes over `u64`.

### Why tagged, and why that is not negotiable

An untagged JSON encoding loses information in four distinct ways, every one of which changes a value's *variant* rather than merely its precision:

| Hazard | Untagged result |
|---|---|
| `Bytes` vs `Str` | Both become JSON strings — indistinguishable on decode |
| `Int` vs `Float` | `Int(1)` and `Float(1.0)` both become `1` |
| `Float(NaN)`, `±Infinity` | No JSON representation → `null`. **Documented on `Value::Float` as a known hazard; this crate is where it gets fixed.** |
| `Int` above 2^53 | JavaScript numbers are IEEE-754 doubles; precision is silently lost. Dart's 64-bit `int` is fine, so **the two guests would disagree** |

The project has repeatedly found that silent normalisation is worse than rejection — the path parser rejects `[007]` for exactly this reason. Tagging costs bytes on a payload that is a handful of small patches, and buys exactness.

### The wire format

```
Null        {"t":"n"}
Bool(b)     {"t":"bool","v":true}
Int(i)      {"t":"i","v":"9007199254740993"}     // STRING — preserves full i64
Float(f)    {"t":"f","v":1.5}
            {"t":"f","v":"NaN"}                   // string sentinels for non-finite
            {"t":"f","v":"Infinity"}
            {"t":"f","v":"-Infinity"}
Str(s)      {"t":"s","v":"hello"}
Bytes(b)    {"t":"b","v":"aGVsbG8="}              // standard base64, with padding
List(l)     {"t":"l","v":[ <encoded>, ... ]}
Map(m)      {"t":"m","v":{ "k": <encoded>, ... }}
```

`Int` is a **string** on the wire. That is deliberate: it is the only way both guests see the same value. The JS client parses it into a `number` when it fits in 2^53 and a `bigint` when it does not; Dart parses it straight into `int`. Encoding it as a JSON number would silently break large values in the webview only, which is the worst kind of bug — one that appears in one guest and not the other.

`Path` encodes as its `Display` string (`"editor.zoom"`, `"documents[3].title"`). Phase 1 proved by exhaustive property test that `parse` and `Display` are mutually inverse over that grammar, so this reuses a guarantee that is already pinned rather than inventing a second representation.

---

## Standing quality bar

Every item below was a real review finding earlier in this project that cost a round trip.

**Documentation**
- Every public item gets a `///` doc comment, **including every enum variant and struct field**. `#![deny(missing_docs)]` enforces it.
- Every `Result`-returning function gets an `# Errors` section.
- Verify doc claims against the code. A review once found docs promising something a type did not deliver.

**Tests**
- No tautological assertions. `assert!(x.is_ok())` is almost never enough — assert the shape.
- **Pin exact counts, not loose bounds.**
- **Property tests pin structure; example tests pin semantics. Include both.** Four algebraic property tests once passed against a mutant that only a concrete example caught.
- **Check a fixture can express the distinctions it polices.** A single-element list once left four mutants alive because "slot i" and "slot 0" were indistinguishable.
- Verify each test genuinely fails before the implementation exists.

**Security — this crate parses untrusted guest input**
- Every decode path must be **total**: malformed input returns an error, never panics, never hangs, never allocates unboundedly.
- No `unwrap`/`expect`/indexing that can panic in non-test code.
- Bound anything guest-controlled that gets echoed into an error message.

**Code**
- Functions under 50 lines.
- `duet-core` must remain **untouched and zero-dependency**.

---

## File Structure

| File | Responsibility |
|---|---|
| `crates/duet-codec/Cargo.toml` | Manifest; `duet-core` + `serde_json` |
| `crates/duet-codec/src/lib.rs` | Crate docs, module decls, re-exports, `Codec` trait |
| `crates/duet-codec/src/error.rs` | `CodecError` |
| `crates/duet-codec/src/base64.rs` | Hand-rolled base64 encode/decode |
| `crates/duet-codec/src/value.rs` | `Value` ↔ `serde_json::Value` |
| `crates/duet-codec/src/wire.rs` | `Path`, `Patch`, `Notification` encoding |
| `crates/duet-codec/src/json.rs` | `JsonCodec` implementing `Codec` |
| `crates/duet-codec/tests/round_trip.rs` | Integration: exhaustive round-trip and adversarial input |

---

## Task 1: Scaffold and `CodecError`

**Files:**
- Create: `crates/duet-codec/Cargo.toml`, `src/lib.rs`, `src/error.rs`
- Modify: root `Cargo.toml`

- [ ] **Step 1: Add to the workspace**

In the root `Cargo.toml`:

```toml
members = ["crates/duet-core", "crates/duet-runtime", "crates/duet-codec"]
```

Leave `exclude = ["spikes"]` untouched.

- [ ] **Step 2: Create the manifest**

Create `crates/duet-codec/Cargo.toml`:

```toml
[package]
name = "duet-codec"
description = "Wire format for Duet: lossless JSON encoding of shared state"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[dependencies]
duet-core = { path = "../duet-core" }
serde_json = "1"
```

`serde_json` is this project's first external dependency, and it is confined to this crate. **`duet-core` must remain zero-dependency** — do not add `serde` to it, and do not derive `Serialize` on its types. Conversion is written by hand here.

- [ ] **Step 3: Write the failing test**

Create `crates/duet-codec/src/error.rs`:

```rust
//! Errors produced when encoding or decoding the wire format.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bad_shape_displays_actionably() {
        let rendered = CodecError::BadShape("missing \"t\"".to_string()).to_string();
        assert!(rendered.contains("\"t\""), "should surface the detail, got: {rendered}");
    }

    #[test]
    fn unknown_tag_names_the_tag() {
        let rendered = CodecError::UnknownTag("q".to_string()).to_string();
        assert!(rendered.contains('q'), "should name the offending tag, got: {rendered}");
    }

    #[test]
    fn guest_supplied_text_is_bounded_in_messages() {
        // This crate parses untrusted guest input. An unbounded echo means a
        // 1 MB tag produces a 1 MB log line.
        let huge = "z".repeat(10_000);
        let rendered = CodecError::UnknownTag(huge.clone()).to_string();
        assert!(
            rendered.len() < 200,
            "guest-supplied text must be truncated in Display, got {} chars",
            rendered.len()
        );
        assert_eq!(
            match CodecError::UnknownTag(huge.clone()) {
                CodecError::UnknownTag(t) => t.len(),
                other => panic!("expected UnknownTag, got {other:?}"),
            },
            10_000,
            "the struct field itself must keep the full value for Debug and tests"
        );
    }

    #[test]
    fn codec_error_is_a_std_error() {
        fn assert_error<E: std::error::Error>() {}
        assert_error::<CodecError>();
    }
}
```

- [ ] **Step 4: Run test to verify it fails**

Run: `cargo test -p duet-codec`
Expected: FAIL — `cannot find type CodecError in this scope`.

- [ ] **Step 5: Write the implementation**

Insert above the test module in `crates/duet-codec/src/error.rs`:

```rust
/// How much guest-supplied text to include in a `Display` message.
///
/// This crate decodes untrusted input, so an unbounded echo would let a guest
/// turn a 1 MB payload into a 1 MB log line. `Debug` and the struct fields keep
/// the full value.
const MAX_ECHO_CHARS: usize = 48;

fn truncated(s: &str) -> String {
    let shown: String = s.chars().take(MAX_ECHO_CHARS).collect();
    if shown.chars().count() < s.chars().count() {
        format!("{shown}…")
    } else {
        shown
    }
}

/// Why a payload could not be encoded or decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum CodecError {
    /// A tagged value carried a `t` this codec does not recognise.
    UnknownTag(String),
    /// A tagged value was structurally wrong — a missing `t`, a missing `v`,
    /// or a payload of the wrong JSON type for its tag.
    BadShape(String),
    /// An `Int` payload was not a valid decimal `i64`.
    BadInt(String),
    /// A `Float` payload was neither a JSON number nor a recognised sentinel
    /// (`"NaN"`, `"Infinity"`, `"-Infinity"`).
    BadFloat(String),
    /// A `Bytes` payload was not valid standard base64.
    BadBase64(String),
    /// A path string did not parse. Carries the rendered parse error, because
    /// `duet_core::PathParseError` byte offsets are more useful to a guest than
    /// a bare failure.
    BadPath(String),
}

impl std::fmt::Display for CodecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CodecError::UnknownTag(t) => write!(f, "unknown type tag \"{}\"", truncated(t)),
            CodecError::BadShape(d) => write!(f, "malformed tagged value: {}", truncated(d)),
            CodecError::BadInt(d) => write!(f, "invalid integer payload \"{}\"", truncated(d)),
            CodecError::BadFloat(d) => write!(f, "invalid float payload \"{}\"", truncated(d)),
            CodecError::BadBase64(d) => write!(f, "invalid base64 payload: {}", truncated(d)),
            CodecError::BadPath(d) => write!(f, "invalid path: {}", truncated(d)),
        }
    }
}

impl std::error::Error for CodecError {}
```

- [ ] **Step 6: Create the crate root**

Create `crates/duet-codec/src/lib.rs`:

```rust
//! Wire format for Duet.
//!
//! Encodes the types that cross the IPC boundary between the Rust host and its
//! Flutter and JavaScript guests. **This crate decodes untrusted input** — every
//! decode path is total: malformed bytes produce a [`CodecError`], never a panic.
//!
//! # Why the encoding is tagged
//!
//! Every value encodes as `{"t":"<tag>","v":<payload>}`. Plain JSON cannot
//! represent [`duet_core::Value`] faithfully: `Bytes` and `Str` would collapse
//! into one JSON string, `Int(1)` and `Float(1.0)` would both become `1`, and
//! `NaN` has no JSON form at all — it would decode back as `Null`, changing the
//! *variant* rather than the magnitude.
//!
//! `Int` is carried as a **string**, not a JSON number, because JavaScript
//! numbers are IEEE-754 doubles: an `i64` above 2^53 would lose precision in the
//! webview while surviving intact in Dart. Two guests disagreeing about a value
//! is the worst kind of bug this format could ship.
//!
//! Verbosity is an accepted cost. Payloads are small patches, guests never see
//! the wire format directly (Phase 4 generates typed accessors over it), and the
//! [`Codec`] trait exists so a compact binary encoding can replace this one
//! without touching a public API.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod error;

pub use error::CodecError;
```

- [ ] **Step 7: Run tests to verify they pass**

Run: `cargo test -p duet-codec`
Expected: PASS — 4 passed.

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml Cargo.lock crates/duet-codec/
git commit -m "feat(codec): scaffold duet-codec with CodecError"
```

---

## Task 2: Base64

Hand-rolled deliberately: base64 is total and unambiguous, unlike JSON, so it does not carry the parser-bug risk that justified `serde_json`. Forty lines beats a second dependency.

**Files:**
- Create: `crates/duet-codec/src/base64.rs`
- Modify: `crates/duet-codec/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/duet-codec/src/base64.rs`:

```rust
//! Standard base64 (RFC 4648) with padding, for `Value::Bytes`.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_the_rfc4648_test_vectors() {
        // From RFC 4648 §10. Pinning a published vector set rather than
        // round-tripping our own output, which would pass even if both
        // directions were wrong in the same way.
        assert_eq!(encode(b""), "");
        assert_eq!(encode(b"f"), "Zg==");
        assert_eq!(encode(b"fo"), "Zm8=");
        assert_eq!(encode(b"foo"), "Zm9v");
        assert_eq!(encode(b"foob"), "Zm9vYg==");
        assert_eq!(encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn decodes_the_rfc4648_test_vectors() {
        assert_eq!(decode("").expect("empty decodes"), b"");
        assert_eq!(decode("Zg==").expect("decodes"), b"f");
        assert_eq!(decode("Zm8=").expect("decodes"), b"fo");
        assert_eq!(decode("Zm9v").expect("decodes"), b"foo");
        assert_eq!(decode("Zm9vYg==").expect("decodes"), b"foob");
        assert_eq!(decode("Zm9vYmE=").expect("decodes"), b"fooba");
        assert_eq!(decode("Zm9vYmFy").expect("decodes"), b"foobar");
    }

    #[test]
    fn round_trips_every_byte_value() {
        let all: Vec<u8> = (0u8..=255).collect();
        assert_eq!(decode(&encode(&all)).expect("decodes"), all);
    }

    #[test]
    fn round_trips_every_length_up_to_the_padding_cycle() {
        // Lengths 0..=8 cover all three padding cases twice over.
        for len in 0..=8usize {
            let bytes: Vec<u8> = (0..len).map(|i| (i * 37 % 256) as u8).collect();
            let encoded = encode(&bytes);
            assert_eq!(
                decode(&encoded).expect("decodes"),
                bytes,
                "round trip failed at length {len} (encoded {encoded:?})"
            );
        }
    }

    #[test]
    fn rejects_malformed_input_without_panicking() {
        // This decodes untrusted guest input. Every one of these must be an
        // error, never a panic and never a silent wrong answer.
        for bad in [
            "Z",          // length 1 mod 4 is impossible
            "Zm9vY",      // length 5, also 1 mod 4
            "Zg=",        // truncated padding
            "Zg===",      // over-padded
            "Zm$v",       // character outside the alphabet
            "Zm9v=",      // padding in a full quantum
            "=Zm9",       // leading padding
            "Zm=v",       // padding in the middle
            "こんにちは",   // multi-byte UTF-8
        ] {
            assert!(
                decode(bad).is_err(),
                "{bad:?} must be rejected, got {:?}",
                decode(bad)
            );
        }
    }

    #[test]
    fn decode_is_total_over_short_arbitrary_strings() {
        // Exhaustive over a small alphabet: no input may panic. This is the
        // property that matters for a decoder facing untrusted input — not that
        // it accepts the right things, but that it never crashes on the wrong
        // ones.
        const ALPHABET: [char; 6] = ['A', 'Z', 'm', '9', '=', '$'];
        let mut checked = 0usize;
        for len in 0..=4usize {
            for mut code in 0..ALPHABET.len().pow(len as u32) {
                let candidate: String = (0..len)
                    .map(|_| {
                        let c = ALPHABET[code % ALPHABET.len()];
                        code /= ALPHABET.len();
                        c
                    })
                    .collect();
                // Must not panic. Either outcome is acceptable.
                let _ = decode(&candidate);
                checked += 1;
            }
        }
        // 6^0 + 6^1 + 6^2 + 6^3 + 6^4 = 1555
        assert_eq!(checked, 1555, "enumeration changed; update deliberately");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p duet-codec`
Expected: FAIL — `cannot find function encode in this scope`.

- [ ] **Step 3: Write the implementation**

Insert above the test module in `crates/duet-codec/src/base64.rs`:

```rust
use crate::error::CodecError;

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Encodes `bytes` as standard base64 with padding.
pub(crate) fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;

        out.push(ALPHABET[(triple >> 18) as usize & 0x3F] as char);
        out.push(ALPHABET[(triple >> 12) as usize & 0x3F] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(triple >> 6) as usize & 0x3F] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[triple as usize & 0x3F] as char
        } else {
            '='
        });
    }
    out
}

/// Maps a base64 character to its 6-bit value, or `None` if it is not in the
/// alphabet.
fn sextet(c: u8) -> Option<u32> {
    match c {
        b'A'..=b'Z' => Some((c - b'A') as u32),
        b'a'..=b'z' => Some((c - b'a') as u32 + 26),
        b'0'..=b'9' => Some((c - b'0') as u32 + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

/// Decodes standard base64 with padding.
///
/// # Errors
///
/// Returns [`CodecError::BadBase64`] for any input that is not exactly
/// well-formed: wrong length, characters outside the alphabet, or misplaced
/// padding. This decodes untrusted guest input, so it is deliberately strict —
/// it never guesses and never panics.
pub(crate) fn decode(s: &str) -> Result<Vec<u8>, CodecError> {
    let bytes = s.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return Err(CodecError::BadBase64(format!(
            "length {} is not a multiple of 4",
            bytes.len()
        )));
    }

    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for (n, quantum) in bytes.chunks(4).enumerate() {
        let is_last = n == bytes.len() / 4 - 1;
        let pad = quantum.iter().filter(|&&c| c == b'=').count();

        if pad > 0 && !is_last {
            return Err(CodecError::BadBase64(
                "padding may appear only in the final quantum".to_string(),
            ));
        }
        if pad > 2 {
            return Err(CodecError::BadBase64(format!("{pad} padding characters")));
        }
        // Padding must be a suffix: "Zg==" is legal, "Z=g=" is not.
        if pad > 0 && quantum[4 - pad..].iter().any(|&c| c != b'=') {
            return Err(CodecError::BadBase64(
                "padding must be at the end of the quantum".to_string(),
            ));
        }

        let mut acc = 0u32;
        for &c in &quantum[..4 - pad] {
            let v = sextet(c).ok_or_else(|| {
                CodecError::BadBase64(format!("character {:?} is not in the alphabet", c as char))
            })?;
            acc = (acc << 6) | v;
        }
        // Left-align the accumulated bits for the bytes we will emit.
        acc <<= 6 * pad;

        out.push((acc >> 16) as u8);
        if pad < 2 {
            out.push((acc >> 8) as u8);
        }
        if pad < 1 {
            out.push(acc as u8);
        }
    }
    Ok(out)
}
```

Note `bytes.len().is_multiple_of(4)` — stable since Rust 1.87, and the workspace requires 1.85. **If it does not compile, use `bytes.len() % 4 != 0` and report that you did.**

- [ ] **Step 4: Declare the module**

Add `mod base64;` to `crates/duet-codec/src/lib.rs`, above `pub mod error;`. It is private.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p duet-codec`
Expected: PASS — 10 passed.

- [ ] **Step 6: Commit**

```bash
git add crates/duet-codec/src/
git commit -m "feat(codec): add strict base64 for Value::Bytes"
```

---

## Task 3: `Value` encoding

**Files:**
- Create: `crates/duet-codec/src/value.rs`
- Modify: `crates/duet-codec/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/duet-codec/src/value.rs`:

```rust
//! `duet_core::Value` to and from tagged `serde_json::Value`.

#[cfg(test)]
mod tests {
    use super::*;
    use duet_core::Value;

    fn json(s: &str) -> serde_json::Value {
        serde_json::from_str(s).expect("test JSON should parse")
    }

    #[test]
    fn encodes_each_variant_with_its_tag() {
        assert_eq!(encode_value(&Value::Null), json(r#"{"t":"n"}"#));
        assert_eq!(encode_value(&Value::Bool(true)), json(r#"{"t":"bool","v":true}"#));
        assert_eq!(encode_value(&Value::Int(42)), json(r#"{"t":"i","v":"42"}"#));
        assert_eq!(encode_value(&Value::Float(1.5)), json(r#"{"t":"f","v":1.5}"#));
        assert_eq!(encode_value(&Value::Str("hi".into())), json(r#"{"t":"s","v":"hi"}"#));
        assert_eq!(
            encode_value(&Value::Bytes(b"foo".to_vec())),
            json(r#"{"t":"b","v":"Zm9v"}"#)
        );
        assert_eq!(
            encode_value(&Value::List(vec![Value::Int(1)])),
            json(r#"{"t":"l","v":[{"t":"i","v":"1"}]}"#)
        );
        assert_eq!(
            encode_value(&Value::map([("k", Value::Bool(false))])),
            json(r#"{"t":"m","v":{"k":{"t":"bool","v":false}}}"#)
        );
    }

    #[test]
    fn int_is_a_string_so_both_guests_agree() {
        // JavaScript numbers are IEEE-754 doubles. Encoded as a JSON number,
        // this value would arrive intact in Dart and corrupted in the webview.
        let big = Value::Int(9_007_199_254_740_993); // 2^53 + 1
        assert_eq!(
            encode_value(&big),
            json(r#"{"t":"i","v":"9007199254740993"}"#)
        );
        assert_eq!(decode_value(&encode_value(&big)).expect("decodes"), big);
    }

    #[test]
    fn non_finite_floats_use_string_sentinels() {
        // Documented on Value::Float as a known hazard: NaN has no JSON form,
        // so an untagged encoding decodes it back as Null — changing the
        // variant, not just the magnitude. These sentinels are the fix.
        assert_eq!(encode_value(&Value::Float(f64::NAN)), json(r#"{"t":"f","v":"NaN"}"#));
        assert_eq!(
            encode_value(&Value::Float(f64::INFINITY)),
            json(r#"{"t":"f","v":"Infinity"}"#)
        );
        assert_eq!(
            encode_value(&Value::Float(f64::NEG_INFINITY)),
            json(r#"{"t":"f","v":"-Infinity"}"#)
        );
    }

    #[test]
    fn nan_round_trips_as_nan_not_null() {
        let decoded = decode_value(&encode_value(&Value::Float(f64::NAN))).expect("decodes");
        match decoded {
            Value::Float(f) => assert!(f.is_nan(), "must still be NaN, got {f}"),
            other => panic!("NaN must stay a Float, got {other:?}"),
        }
        // Note: NaN != NaN, so this cannot be asserted with assert_eq!. That
        // non-reflexivity is documented on Value::Float.
    }

    #[test]
    fn bytes_and_str_stay_distinguishable() {
        // The single clearest reason the encoding is tagged.
        let s = Value::Str("foo".into());
        let b = Value::Bytes(b"foo".to_vec());
        assert_ne!(encode_value(&s), encode_value(&b));
        assert_eq!(decode_value(&encode_value(&s)).expect("decodes"), s);
        assert_eq!(decode_value(&encode_value(&b)).expect("decodes"), b);
    }

    #[test]
    fn int_and_float_stay_distinguishable() {
        let i = Value::Int(1);
        let f = Value::Float(1.0);
        assert_ne!(encode_value(&i), encode_value(&f));
        assert_eq!(decode_value(&encode_value(&i)).expect("decodes"), i);
        assert_eq!(decode_value(&encode_value(&f)).expect("decodes"), f);
    }

    #[test]
    fn decode_rejects_malformed_shapes_without_panicking() {
        for bad in [
            r#"42"#,                       // not an object
            r#"{}"#,                       // no tag
            r#"{"t":"q","v":1}"#,          // unknown tag
            r#"{"t":"i"}"#,                // missing payload
            r#"{"t":"i","v":42}"#,         // Int payload must be a string
            r#"{"t":"i","v":"nope"}"#,     // not a decimal integer
            r#"{"t":"i","v":"999999999999999999999"}"#, // overflows i64
            r#"{"t":"f","v":"huge"}"#,     // unrecognised float sentinel
            r#"{"t":"b","v":"!!!"}"#,      // invalid base64
            r#"{"t":"bool","v":"yes"}"#,   // wrong payload type
            r#"{"t":"l","v":{}}"#,         // List payload must be an array
            r#"{"t":"m","v":[]}"#,         // Map payload must be an object
            r#"{"t":5,"v":1}"#,            // tag must be a string
        ] {
            let parsed = json(bad);
            assert!(
                decode_value(&parsed).is_err(),
                "{bad} must be rejected, got {:?}",
                decode_value(&parsed)
            );
        }
    }

    #[test]
    fn deeply_nested_input_does_not_overflow_the_stack() {
        // A guest can send arbitrarily nested JSON. serde_json has its own
        // recursion limit; this pins that we surface it as an error rather
        // than a crash.
        let deep = format!(
            "{}{}{}",
            r#"{"t":"l","v":["#.repeat(200),
            r#"{"t":"n"}"#,
            r#"]}"#.repeat(200)
        );
        match serde_json::from_str::<serde_json::Value>(&deep) {
            Ok(parsed) => {
                // Parsed fine; decoding must also not panic.
                let _ = decode_value(&parsed);
            }
            Err(_) => {
                // serde_json rejected it first, which is also acceptable.
            }
        }
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p duet-codec`
Expected: FAIL — `cannot find function encode_value in this scope`.

- [ ] **Step 3: Write the implementation**

Insert above the test module in `crates/duet-codec/src/value.rs`:

```rust
use std::collections::BTreeMap;

use duet_core::Value;
use serde_json::{Map as JsonMap, Value as Json};

use crate::base64;
use crate::error::CodecError;

/// Builds a `{"t":tag,"v":payload}` object.
fn tagged(tag: &str, payload: Json) -> Json {
    let mut m = JsonMap::new();
    m.insert("t".to_string(), Json::String(tag.to_string()));
    m.insert("v".to_string(), payload);
    Json::Object(m)
}

/// Encodes a [`Value`] into its tagged JSON form.
///
/// Total: every `Value` has an encoding, including non-finite floats.
pub(crate) fn encode_value(value: &Value) -> Json {
    match value {
        Value::Null => {
            let mut m = JsonMap::new();
            m.insert("t".to_string(), Json::String("n".to_string()));
            Json::Object(m)
        }
        Value::Bool(b) => tagged("bool", Json::Bool(*b)),
        // A string, so an i64 above 2^53 survives the JavaScript side intact.
        Value::Int(i) => tagged("i", Json::String(i.to_string())),
        Value::Float(f) => tagged("f", encode_float(*f)),
        Value::Str(s) => tagged("s", Json::String(s.clone())),
        Value::Bytes(b) => tagged("b", Json::String(base64::encode(b))),
        Value::List(items) => tagged("l", Json::Array(items.iter().map(encode_value).collect())),
        Value::Map(entries) => {
            let mut m = JsonMap::new();
            for (k, v) in entries {
                m.insert(k.clone(), encode_value(v));
            }
            tagged("m", Json::Object(m))
        }
    }
}

/// JSON has no representation for non-finite floats, so they travel as string
/// sentinels. Without this they would decode back as `Null`, changing the
/// value's variant rather than its magnitude.
fn encode_float(f: f64) -> Json {
    if f.is_nan() {
        return Json::String("NaN".to_string());
    }
    if f == f64::INFINITY {
        return Json::String("Infinity".to_string());
    }
    if f == f64::NEG_INFINITY {
        return Json::String("-Infinity".to_string());
    }
    serde_json::Number::from_f64(f)
        .map(Json::Number)
        .unwrap_or_else(|| Json::String("NaN".to_string()))
}

/// Decodes a tagged JSON value.
///
/// # Errors
///
/// Returns a [`CodecError`] describing the first structural problem found.
/// Total over all JSON input: never panics, whatever a guest sends.
pub(crate) fn decode_value(json: &Json) -> Result<Value, CodecError> {
    let obj = json
        .as_object()
        .ok_or_else(|| CodecError::BadShape(format!("expected an object, found {json}")))?;
    let tag = obj
        .get("t")
        .ok_or_else(|| CodecError::BadShape("missing \"t\"".to_string()))?
        .as_str()
        .ok_or_else(|| CodecError::BadShape("\"t\" must be a string".to_string()))?;

    if tag == "n" {
        return Ok(Value::Null);
    }

    let payload = obj
        .get("v")
        .ok_or_else(|| CodecError::BadShape(format!("tag \"{tag}\" requires \"v\"")))?;

    match tag {
        "bool" => payload
            .as_bool()
            .map(Value::Bool)
            .ok_or_else(|| CodecError::BadShape("\"bool\" payload must be a boolean".to_string())),
        "i" => {
            let s = payload.as_str().ok_or_else(|| {
                CodecError::BadInt("payload must be a decimal string".to_string())
            })?;
            s.parse::<i64>()
                .map(Value::Int)
                .map_err(|_| CodecError::BadInt(s.to_string()))
        }
        "f" => decode_float(payload),
        "s" => payload
            .as_str()
            .map(|s| Value::Str(s.to_string()))
            .ok_or_else(|| CodecError::BadShape("\"s\" payload must be a string".to_string())),
        "b" => {
            let s = payload.as_str().ok_or_else(|| {
                CodecError::BadBase64("payload must be a string".to_string())
            })?;
            base64::decode(s).map(Value::Bytes)
        }
        "l" => {
            let arr = payload
                .as_array()
                .ok_or_else(|| CodecError::BadShape("\"l\" payload must be an array".to_string()))?;
            arr.iter().map(decode_value).collect::<Result<_, _>>().map(Value::List)
        }
        "m" => {
            let obj = payload
                .as_object()
                .ok_or_else(|| CodecError::BadShape("\"m\" payload must be an object".to_string()))?;
            let mut out = BTreeMap::new();
            for (k, v) in obj {
                out.insert(k.clone(), decode_value(v)?);
            }
            Ok(Value::Map(out))
        }
        other => Err(CodecError::UnknownTag(other.to_string())),
    }
}

fn decode_float(payload: &Json) -> Result<Value, CodecError> {
    if let Some(n) = payload.as_f64() {
        return Ok(Value::Float(n));
    }
    match payload.as_str() {
        Some("NaN") => Ok(Value::Float(f64::NAN)),
        Some("Infinity") => Ok(Value::Float(f64::INFINITY)),
        Some("-Infinity") => Ok(Value::Float(f64::NEG_INFINITY)),
        Some(other) => Err(CodecError::BadFloat(other.to_string())),
        None => Err(CodecError::BadFloat(
            "payload must be a number or a sentinel string".to_string(),
        )),
    }
}

#[cfg(test)]
pub(crate) use decode_value as decode_value_for_tests;
```

Remove that last `pub(crate) use` line if it triggers an unused warning — it is only there in case a later task needs it. **Report whether you kept it.**

- [ ] **Step 4: Declare the module**

Add `mod value;` to `crates/duet-codec/src/lib.rs`. Private for now.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p duet-codec`
Expected: PASS — 18 passed.

- [ ] **Step 6: Commit**

```bash
git add crates/duet-codec/src/
git commit -m "feat(codec): encode Value with a lossless tagged form"
```

---

## Task 4: Round-trip property test

The single most important test in the crate. Everything above is an example; this pins the invariant.

**Files:**
- Create: `crates/duet-codec/tests/round_trip.rs`
- Modify: `crates/duet-codec/src/lib.rs` (to expose what the test needs)

- [ ] **Step 1: Expose an encode/decode pair**

`encode_value`/`decode_value` are `pub(crate)`. The integration test needs a public entry point, and the `Codec` trait is not written yet, so add a minimal public API to `crates/duet-codec/src/lib.rs`:

```rust
mod base64;
pub mod error;
mod value;

pub use error::CodecError;

/// Encodes a [`duet_core::Value`] into its tagged JSON representation.
pub fn encode_value(value: &duet_core::Value) -> serde_json::Value {
    value::encode_value(value)
}

/// Decodes a tagged JSON representation back into a [`duet_core::Value`].
///
/// # Errors
///
/// Returns a [`CodecError`] describing the first structural problem found. This
/// function is total over all JSON input: it never panics, whatever a guest
/// sends.
pub fn decode_value(json: &serde_json::Value) -> Result<duet_core::Value, CodecError> {
    value::decode_value(json)
}
```

- [ ] **Step 2: Write the failing test**

Create `crates/duet-codec/tests/round_trip.rs`:

```rust
//! The invariant the whole crate exists to provide: decode(encode(v)) == v.

use duet_core::Value;
use duet_codec::{decode_value, encode_value};

/// Every structurally distinct `Value` shape, built by enumeration rather than
/// hand-listing. A hand-written list omits exactly the case that breaks.
fn corpus() -> Vec<Value> {
    let scalars = vec![
        Value::Null,
        Value::Bool(true),
        Value::Bool(false),
        Value::Int(0),
        Value::Int(-1),
        Value::Int(i64::MAX),
        Value::Int(i64::MIN),
        Value::Int(9_007_199_254_740_993), // 2^53 + 1, unsafe in JS as a number
        Value::Float(0.0),
        Value::Float(-0.0),
        Value::Float(1.5),
        Value::Float(f64::MIN),
        Value::Float(f64::MAX),
        Value::Float(f64::EPSILON),
        Value::Str(String::new()),
        Value::Str("hello".into()),
        Value::Str("café 🦀 \u{202e}".into()), // multi-byte, emoji, RTL override
        Value::Str("\"quotes\" and \\backslashes\\".into()),
        Value::Bytes(Vec::new()),
        Value::Bytes(vec![0]),
        Value::Bytes((0u8..=255).collect()),
    ];

    let mut all = scalars.clone();
    // One level of nesting over every scalar, in both container kinds.
    all.push(Value::List(scalars.clone()));
    all.push(Value::Map(
        scalars
            .iter()
            .enumerate()
            .map(|(i, v)| (format!("k{i}"), v.clone()))
            .collect(),
    ));
    // Two levels, to catch a decoder that only recurses once.
    all.push(Value::List(vec![Value::List(scalars.clone())]));
    all.push(Value::map([("outer", Value::map([("inner", Value::Int(7))]))]));
    // Empty containers — distinct from absent, and a common off-by-one.
    all.push(Value::List(Vec::new()));
    all.push(Value::map([]));
    all
}

#[test]
fn every_value_round_trips_exactly() {
    let corpus = corpus();
    assert_eq!(corpus.len(), 27, "corpus size changed; update deliberately");

    for original in &corpus {
        let encoded = encode_value(original);
        let decoded = decode_value(&encoded).unwrap_or_else(|e| {
            panic!("decode failed for {original:?}: {e}");
        });
        assert_eq!(
            &decoded, original,
            "round trip changed the value\n  encoded as: {encoded}"
        );
    }
}

#[test]
fn round_trip_survives_a_serialized_text_hop() {
    // The real path is Rust -> JSON text -> guest -> JSON text -> Rust, not
    // Rust -> serde_json::Value -> Rust. Serializing to a string and back
    // exercises number formatting, escaping and precision, which the in-memory
    // round trip skips entirely.
    for original in corpus() {
        let text = serde_json::to_string(&encode_value(&original)).expect("encodes to text");
        let reparsed: serde_json::Value = serde_json::from_str(&text).expect("parses back");
        let decoded = decode_value(&reparsed).unwrap_or_else(|e| {
            panic!("decode failed for {original:?} via text {text}: {e}");
        });
        assert_eq!(&decoded, &original, "text round trip changed the value: {text}");
    }
}

#[test]
fn nan_round_trips_through_text_as_nan() {
    // Cannot go in the corpus: NaN != NaN, so assert_eq! would fail on a
    // correct implementation. That non-reflexivity is documented on
    // Value::Float.
    let text = serde_json::to_string(&encode_value(&Value::Float(f64::NAN))).expect("encodes");
    let reparsed: serde_json::Value = serde_json::from_str(&text).expect("parses");
    match decode_value(&reparsed).expect("decodes") {
        Value::Float(f) => assert!(f.is_nan(), "expected NaN, got {f}"),
        other => panic!("NaN must remain a Float, got {other:?}"),
    }
}

#[test]
fn decode_never_panics_on_arbitrary_json() {
    // Exhaustive over short JSON-ish strings. The property that matters for a
    // decoder facing untrusted guest input is not that it accepts the right
    // things — it is that it never crashes on the wrong ones.
    const ALPHABET: [char; 8] = ['{', '}', '"', 't', ':', '1', '[', ']'];
    let mut parsed_ok = 0usize;
    let mut checked = 0usize;

    for len in 0..=5usize {
        for mut code in 0..ALPHABET.len().pow(len as u32) {
            let candidate: String = (0..len)
                .map(|_| {
                    let c = ALPHABET[code % ALPHABET.len()];
                    code /= ALPHABET.len();
                    c
                })
                .collect();
            checked += 1;
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&candidate) {
                parsed_ok += 1;
                // Must not panic. Either outcome is fine.
                let _ = decode_value(&json);
            }
        }
    }

    // 8^0 + 8^1 + ... + 8^5 = 37449
    assert_eq!(checked, 37_449, "enumeration changed; update deliberately");
    assert!(
        parsed_ok > 0,
        "the alphabet must produce some valid JSON or this test proves nothing"
    );
}
```

- [ ] **Step 3: Run tests to verify they pass**

Run: `cargo test -p duet-codec --test round_trip`
Expected: PASS — 4 passed. If the corpus length assertion fails, correct the number to the real one rather than removing the assertion.

- [ ] **Step 4: Commit**

```bash
git add crates/duet-codec/
git commit -m "test(codec): prove every Value round-trips exactly, including through text"
```

---

## Task 5: `Path`, `Patch` and `Notification`

**Files:**
- Create: `crates/duet-codec/src/wire.rs`
- Modify: `crates/duet-codec/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/duet-codec/src/wire.rs`:

```rust
//! Encoding for the addressed types: `Path`, `Patch`, `Notification`.

#[cfg(test)]
mod tests {
    use super::*;
    use duet_core::{Notification, Patch, Path, SubscriberId, SubscriptionId, Value};

    fn json(s: &str) -> serde_json::Value {
        serde_json::from_str(s).expect("test JSON should parse")
    }

    fn p(s: &str) -> Path {
        Path::parse(s).expect("test path should parse")
    }

    #[test]
    fn path_encodes_as_its_display_string() {
        // Reuses duet-core's proven parse/Display inverse rather than
        // inventing a second representation.
        assert_eq!(encode_path(&p("editor.zoom")), json(r#""editor.zoom""#));
        assert_eq!(encode_path(&p("documents[3].title")), json(r#""documents[3].title""#));
        assert_eq!(encode_path(&Path::root()), json(r#""""#));
    }

    #[test]
    fn path_round_trips_including_root_and_indices() {
        for raw in ["", "editor.zoom", "documents[3].title", "a[0][1].b", "café.zoom"] {
            let path = p(raw);
            let decoded = decode_path(&encode_path(&path)).expect("decodes");
            assert_eq!(decoded, path, "round trip failed for {raw:?}");
        }
    }

    #[test]
    fn decode_path_rejects_malformed_strings() {
        for bad in [r#""foo]""#, r#""a.[0]""#, r#""a[007]""#, r#""foo[""#, r#"42"#] {
            assert!(
                decode_path(&json(bad)).is_err(),
                "{bad} must be rejected, got {:?}",
                decode_path(&json(bad))
            );
        }
    }

    #[test]
    fn patch_carries_path_and_value() {
        let patch = Patch {
            path: p("editor.zoom"),
            value: Value::Float(1.5),
        };
        assert_eq!(
            encode_patch(&patch),
            json(r#"{"path":"editor.zoom","value":{"t":"f","v":1.5}}"#)
        );
        assert_eq!(decode_patch(&encode_patch(&patch)).expect("decodes"), patch);
    }

    #[test]
    fn notification_carries_both_ids_as_strings() {
        // u64 ids exceed JavaScript's safe integer range just as i64 does, so
        // they travel as strings for the same reason.
        let note = Notification {
            subscriber: SubscriberId(u64::MAX),
            subscription: SubscriptionId(7),
            patch: Patch {
                path: p("a"),
                value: Value::Null,
            },
        };
        let encoded = encode_notification(&note);
        assert_eq!(
            encoded,
            json(
                r#"{"subscriber":"18446744073709551615","subscription":"7",
                    "patch":{"path":"a","value":{"t":"n"}}}"#
            )
        );
        assert_eq!(decode_notification(&encoded).expect("decodes"), note);
    }

    #[test]
    fn decode_rejects_malformed_patches_and_notifications() {
        for bad in [r#"{}"#, r#"{"path":"a"}"#, r#"{"value":{"t":"n"}}"#, r#"[]"#] {
            assert!(decode_patch(&json(bad)).is_err(), "{bad} must be rejected");
        }
        for bad in [
            r#"{}"#,
            r#"{"subscriber":"1"}"#,
            r#"{"subscriber":1,"subscription":"1","patch":{"path":"a","value":{"t":"n"}}}"#,
            r#"{"subscriber":"x","subscription":"1","patch":{"path":"a","value":{"t":"n"}}}"#,
        ] {
            assert!(
                decode_notification(&json(bad)).is_err(),
                "{bad} must be rejected"
            );
        }
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p duet-codec`
Expected: FAIL — `cannot find function encode_path in this scope`.

- [ ] **Step 3: Write the implementation**

Insert above the test module in `crates/duet-codec/src/wire.rs`:

```rust
use duet_core::{Notification, Patch, Path, SubscriberId, SubscriptionId};
use serde_json::{Map as JsonMap, Value as Json};

use crate::error::CodecError;
use crate::value::{decode_value, encode_value};

/// Encodes a [`Path`] as its `Display` string.
///
/// `duet-core` proves by exhaustive property test that `Path::parse` and
/// `Display` are mutually inverse, so this reuses a guarantee that is already
/// pinned rather than introducing a second representation to keep in sync.
pub(crate) fn encode_path(path: &Path) -> Json {
    Json::String(path.to_string())
}

/// Decodes a path string.
///
/// # Errors
///
/// [`CodecError::BadShape`] if the JSON is not a string, or
/// [`CodecError::BadPath`] carrying the rendered parse error — whose byte
/// offsets are more actionable for a guest than a bare failure.
pub(crate) fn decode_path(json: &Json) -> Result<Path, CodecError> {
    let s = json
        .as_str()
        .ok_or_else(|| CodecError::BadShape("path must be a string".to_string()))?;
    Path::parse(s).map_err(|e| CodecError::BadPath(e.to_string()))
}

/// Reads a required field.
fn field<'a>(obj: &'a JsonMap<String, Json>, name: &str) -> Result<&'a Json, CodecError> {
    obj.get(name)
        .ok_or_else(|| CodecError::BadShape(format!("missing \"{name}\"")))
}

/// Reads a `u64` carried as a decimal string.
fn u64_field(obj: &JsonMap<String, Json>, name: &str) -> Result<u64, CodecError> {
    let s = field(obj, name)?
        .as_str()
        .ok_or_else(|| CodecError::BadShape(format!("\"{name}\" must be a decimal string")))?;
    s.parse::<u64>()
        .map_err(|_| CodecError::BadInt(format!("\"{name}\": {s}")))
}

fn as_object<'a>(json: &'a Json, what: &str) -> Result<&'a JsonMap<String, Json>, CodecError> {
    json.as_object()
        .ok_or_else(|| CodecError::BadShape(format!("{what} must be an object")))
}

/// Encodes a [`Patch`].
pub(crate) fn encode_patch(patch: &Patch) -> Json {
    let mut m = JsonMap::new();
    m.insert("path".to_string(), encode_path(&patch.path));
    m.insert("value".to_string(), encode_value(&patch.value));
    Json::Object(m)
}

/// Decodes a [`Patch`].
///
/// # Errors
///
/// A [`CodecError`] describing the first structural problem found.
pub(crate) fn decode_patch(json: &Json) -> Result<Patch, CodecError> {
    let obj = as_object(json, "patch")?;
    Ok(Patch {
        path: decode_path(field(obj, "path")?)?,
        value: decode_value(field(obj, "value")?)?,
    })
}

/// Encodes a [`Notification`].
///
/// Both ids travel as decimal strings: `u64` exceeds JavaScript's safe integer
/// range just as `i64` does, and an id that differs between the two guests
/// would misroute notifications.
pub(crate) fn encode_notification(note: &Notification) -> Json {
    let mut m = JsonMap::new();
    m.insert(
        "subscriber".to_string(),
        Json::String(note.subscriber.0.to_string()),
    );
    m.insert(
        "subscription".to_string(),
        Json::String(note.subscription.0.to_string()),
    );
    m.insert("patch".to_string(), encode_patch(&note.patch));
    Json::Object(m)
}

/// Decodes a [`Notification`].
///
/// # Errors
///
/// A [`CodecError`] describing the first structural problem found.
pub(crate) fn decode_notification(json: &Json) -> Result<Notification, CodecError> {
    let obj = as_object(json, "notification")?;
    Ok(Notification {
        subscriber: SubscriberId(u64_field(obj, "subscriber")?),
        subscription: SubscriptionId(u64_field(obj, "subscription")?),
        patch: decode_patch(field(obj, "patch")?)?,
    })
}
```

- [ ] **Step 4: Declare and re-export**

In `crates/duet-codec/src/lib.rs` add `mod wire;` and public wrappers mirroring the `encode_value`/`decode_value` pair already there:

```rust
/// Encodes a [`duet_core::Patch`].
pub fn encode_patch(patch: &duet_core::Patch) -> serde_json::Value {
    wire::encode_patch(patch)
}

/// Decodes a [`duet_core::Patch`].
///
/// # Errors
///
/// A [`CodecError`] describing the first structural problem found.
pub fn decode_patch(json: &serde_json::Value) -> Result<duet_core::Patch, CodecError> {
    wire::decode_patch(json)
}

/// Encodes a [`duet_core::Notification`].
pub fn encode_notification(note: &duet_core::Notification) -> serde_json::Value {
    wire::encode_notification(note)
}

/// Decodes a [`duet_core::Notification`].
///
/// # Errors
///
/// A [`CodecError`] describing the first structural problem found.
pub fn decode_notification(
    json: &serde_json::Value,
) -> Result<duet_core::Notification, CodecError> {
    wire::decode_notification(json)
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p duet-codec`
Expected: PASS — 24 unit + 4 integration.

- [ ] **Step 6: Commit**

```bash
git add crates/duet-codec/src/
git commit -m "feat(codec): encode Path, Patch and Notification"
```

---

## Task 6: Coverage gate and CI

**Files:**
- Modify: `.github/workflows/duet.yml`

- [ ] **Step 1: Measure coverage**

Run: `cargo llvm-cov -p duet-codec --summary-only`

`cargo-llvm-cov` 0.8.7 is already installed. This forces an instrumented rebuild taking a few minutes — be patient.

Report the real per-file numbers. If any file is below 90% line coverage, read the report and add tests for those branches. **Do not lower the threshold.** If a line is genuinely unreachable, say so explicitly rather than contorting a test.

- [ ] **Step 2: Confirm the workspace gate still passes**

Run: `cargo llvm-cov --workspace --locked --fail-under-lines 90`
Expected: exit 0. Report the workspace total.

- [ ] **Step 3: CI needs no structural change**

`.github/workflows/duet.yml` already runs `--workspace` for clippy, coverage, docs and the single-threaded test pass, so the new crate is gated automatically. **Verify this by reading the file** and confirm every step uses `--workspace`. If any step still names a specific crate, fix it.

- [ ] **Step 4: Verify every CI step locally**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked
cargo llvm-cov --workspace --locked --fail-under-lines 90
cargo test --workspace --locked -- --test-threads=1
```

- [ ] **Step 5: Commit**

```bash
git add .github/workflows/duet.yml crates/ Cargo.lock
git commit -m "ci: gate duet-codec alongside the rest of the workspace"
```

---

## Done criteria

- [ ] `cargo test --workspace` passes — report exact counts per crate
- [ ] `cargo test --workspace -- --test-threads=1` passes with identical counts
- [ ] `cargo llvm-cov --workspace --fail-under-lines 90` exits 0
- [ ] `cargo clippy --workspace --all-targets --locked -- -D warnings` clean
- [ ] `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps` clean
- [ ] `cargo fmt --all -- --check` clean
- [ ] **`duet-core` is still zero-dependency** — `cargo tree -p duet-core` shows no dependencies
- [ ] **`duet-core` is unchanged** — `git diff --stat main -- crates/duet-core` is empty
- [ ] `duet-codec`'s only dependencies are `duet-core` and `serde_json`
- [ ] No `unwrap`/`expect` in non-test code anywhere in `duet-codec`
- [ ] Every `Value` in the corpus round-trips exactly, including through serialized text

## What Phase 2b-0 deliberately does not build

- **The `Codec` trait and a binary codec.** One implementation exists, so a trait abstracting it would be speculative. The free functions are the API until a second encoding needs to slot in; the spec's "drop-in replacement" claim survives because callers depend on functions, not a concrete type.
- **The request/response message envelope.** Guest→host commands (`get`, `set`, `subscribe`) need to be framed, but the framing is transport-shaped — Tauri IPC and the Flutter platform channel differ — so it belongs with the transport in 2b-1, not here.
- **A TypeScript or Dart decoder.** Those ship with their clients.
- **Compression or chunking.** No benchmark exists. Spec §6.3 says optimising before one would be guesswork.
