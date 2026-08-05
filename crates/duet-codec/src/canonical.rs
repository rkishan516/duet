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
//! `fixtures/duet_guest/lib/duet_value.dart`, and `duet-protocol` gates every
//! `u64` wire field on [`is_canonical_unsigned_digits`].

/// True if `s` is a canonical, sign-free decimal digit string: at least one
/// digit, and no leading zero unless `s` is exactly `"0"`.
///
/// This is the rule for every **unsigned** integer on the wire — the `id`,
/// `subscription` and `subscriber` fields. Rejects `""`, `"+1"`, `"007"`,
/// `" 1"`, `"1 "`, `"1_000"` and anything non-numeric; accepts `"0"` and
/// `"18446744073709551615"`.
///
/// Note this validates *spelling only*. A string can be canonical and still
/// overflow the target integer type, so callers must still check the result
/// of `str::parse`.
pub fn is_canonical_unsigned_digits(s: &str) -> bool {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    s == "0" || !s.starts_with('0')
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
}
