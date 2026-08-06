//! Canonical-decimal validation for integer payloads.
//!
//! `i64`/`u64`'s `FromStr` accepts more than the wire format should: a
//! leading `+`, and leading zeros like `"007"`. Left unchecked, two different
//! guest-sent strings (`"7"` and `"007"`) would decode to the same value and
//! then both re-encode as a third (`"7"`) — the same class of ambiguity the
//! base64 decoder and `duet-core`'s path parser (which rejects `[007]`)
//! already refuse. These functions gate `str::parse` so only the one
//! canonical rendering of each integer is accepted.
//!
//! # Part of the wire contract, not an implementation detail
//!
//! **Every integer on the Duet wire travels as a canonical decimal string**,
//! and every decoder must reject any other spelling of the same number. This
//! is not a nicety: correlation ids (`id`, `subscription`, `subscriber`) are
//! *echoed* by the host in canonical form, and a guest that keys its pending
//! map by the string it sent will never match a reply spelled differently.
//! The failure is a silent hang — no error, just a promise that never
//! settles.
//!
//! These predicates are public so third-party guest implementations in Rust
//! can enforce the identical rule from the one definition of it, rather than
//! writing a fourth near-copy. The Dart guest client mirrors them in
//! `packages/duet/lib/src/duet_json.dart`, and `duet-protocol` gates every
//! `u64` wire field on [`parse_wire_id`].

/// True if `s` is a canonical, sign-free decimal digit string: at least one
/// digit, and no leading zero unless `s` is exactly `"0"`.
///
/// This is the rule for every **unsigned** integer on the wire — the `id`,
/// `subscription` and `subscriber` fields. Rejects `""`, `"+1"`, `"007"`,
/// `" 1"`, `"1 "`, `"1_000"` and anything non-numeric; accepts `"0"` and
/// `"18446744073709551615"`.
///
/// Note this validates *spelling only* — it says nothing about magnitude.
/// `"18446744073709551615"` is spelled canonically and is still outside the
/// wire's id domain, so decoders must go through [`parse_wire_id`] rather than
/// pairing this predicate with a bare `str::parse`.
pub fn is_canonical_unsigned_digits(s: &str) -> bool {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    s == "0" || !s.starts_with('0')
}

/// The inclusive upper bound of the Duet wire's id domain: `i64::MAX`
/// (`9223372036854775807`) — deliberately **not** `u64::MAX`.
///
/// # Why the domain is narrower than the Rust type
///
/// Dart's native `int` is a 64-bit **signed** integer, and JavaScript's
/// `BigInt`-free `Number` is narrower still. `int.tryParse` returns `null` for
/// `"9223372036854775808"`, so a host that emitted an id above this bound
/// would emit one no Dart guest can read — the guest rejects the reply as
/// malformed and the call fails, or worse, hangs. Java, Kotlin and Swift guests
/// would all hit the same wall.
///
/// Narrowing here costs nothing measurable: ids are allocated sequentially
/// from 1 (`RequestId`) or 0 (`duet_core::Store`'s subscription counter), so
/// reaching `i64::MAX` needs ~9.2 × 10^18 requests on one connection. In
/// exchange, **one** id domain holds in every guest language, which is what a
/// cross-language golden corpus needs to be able to assert.
///
/// The alternative — widening Dart to `BigInt` — was rejected: it would put
/// `BigInt` in the public Dart API for a range unreachable in practice, and
/// force the same on every future 64-bit-signed guest language.
pub const MAX_WIRE_ID: u64 = i64::MAX as u64;

/// Parses one of the wire's id fields — `id`, `subscription`, `subscriber` —
/// enforcing **both** the canonical spelling and the domain `0..=`[`MAX_WIRE_ID`].
///
/// Returns `None` for any string that is not the one canonical rendering of an
/// integer in that domain. This is the single definition every Duet decoder
/// must route its id fields through: `duet-codec`'s `u64_field`,
/// `duet-protocol`'s `u64_field` and its text-level id recovery all call it, so
/// there is no second place for the two rules to drift apart.
///
/// The wire types stay `u64`; only this decoder narrows. Encoders emit an id
/// faithfully whatever its magnitude, so an out-of-domain id is refused at the
/// first decode — loudly and identically in every implementation — rather than
/// being clamped into a different id or silently accepted by some guests only.
pub fn parse_wire_id(s: &str) -> Option<u64> {
    if !is_canonical_unsigned_digits(s) {
        return None;
    }
    match s.parse::<u64>() {
        Ok(n) if n <= MAX_WIRE_ID => Some(n),
        _ => None,
    }
}

/// True if `s` is the canonical decimal rendering of some `i64`: no leading
/// `+`, no leading zeros, and no `-0` — canonical zero is `"0"`.
///
/// This is the rule for the **signed** integer payload of a `{"t":"i"}`
/// value. `-0` is rejected deliberately: `i64` has no negative zero, so
/// admitting the spelling would give one value two renderings.
///
/// Note this validates *spelling only* — see [`is_canonical_unsigned_digits`].
pub fn is_canonical_signed_decimal(s: &str) -> bool {
    match s.strip_prefix('-') {
        Some(magnitude) => magnitude != "0" && is_canonical_unsigned_digits(magnitude),
        None => is_canonical_unsigned_digits(s),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_canonical_unsigned_forms() {
        for good in ["0", "1", "7", "42", "18446744073709551615"] {
            assert!(
                is_canonical_unsigned_digits(good),
                "{good} should be canonical"
            );
        }
    }

    #[test]
    fn rejects_non_canonical_unsigned_forms() {
        for bad in [
            "", "+1", "007", "00", "-1", "1.0", "1e3", "0x1", " 1", "1 ", "abc",
        ] {
            assert!(
                !is_canonical_unsigned_digits(bad),
                "{bad} should be rejected"
            );
        }
    }

    #[test]
    fn accepts_canonical_signed_forms() {
        for good in [
            "0",
            "1",
            "-1",
            "9223372036854775807",
            "-9223372036854775808",
        ] {
            assert!(
                is_canonical_signed_decimal(good),
                "{good} should be canonical"
            );
        }
    }

    #[test]
    fn rejects_non_canonical_signed_forms() {
        for bad in ["+5", "007", "-007", "-0", "", "1.0", "--1"] {
            assert!(
                !is_canonical_signed_decimal(bad),
                "{bad} should be rejected"
            );
        }
    }

    #[test]
    fn the_wire_id_domain_stops_at_i64_max() {
        // Dart's native int is 64-bit SIGNED: int.tryParse("9223372036854775808")
        // returns null, so a Dart guest cannot read an id the host could emit.
        // Narrowing here costs nothing (ids are sequential from 1) and makes one
        // id domain hold in every guest language.
        assert_eq!(MAX_WIRE_ID, 9_223_372_036_854_775_807);
        for (raw, expected) in [
            ("0", 0u64),
            ("1", 1),
            ("9007199254740993", 9_007_199_254_740_993),
            ("9223372036854775807", MAX_WIRE_ID),
        ] {
            assert_eq!(
                parse_wire_id(raw),
                Some(expected),
                "id {raw:?} is within the domain"
            );
        }
        for raw in [
            "9223372036854775808",  // i64::MAX + 1
            "18446744073709551615", // u64::MAX
            "99999999999999999999", // overflows u64 outright
        ] {
            assert_eq!(parse_wire_id(raw), None, "id {raw:?} is outside the domain");
        }
    }

    #[test]
    fn the_wire_id_parser_still_enforces_the_canonical_spelling() {
        // Domain and spelling are two independent rules and this parser is the
        // only place both are applied; a refactor that dropped either would
        // otherwise leave the other still passing its own test.
        for bad in [
            "007",
            "+1",
            "0000000000000000000007",
            " 1",
            "1 ",
            "1_000",
            "",
        ] {
            assert_eq!(
                parse_wire_id(bad),
                None,
                "id {bad:?} must be rejected as non-canonical"
            );
        }
    }
}
