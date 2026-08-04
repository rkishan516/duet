//! Canonical-decimal validation for integer payloads.
//!
//! `i64`/`u64`'s `FromStr` accepts more than the wire format should: a
//! leading `+`, and leading zeros like `"007"`. Left unchecked, two different
//! guest-sent strings (`"7"` and `"007"`) would decode to the same value and
//! then both re-encode as a third (`"7"`) — the same class of ambiguity the
//! base64 decoder and `duet-core`'s path parser (which rejects `[007]`)
//! already refuse. These functions gate `str::parse` so only the one
//! canonical rendering of each integer is accepted.

/// True if `s` is a canonical, sign-free decimal digit string: at least one
/// digit, and no leading zero unless `s` is exactly `"0"`.
pub(crate) fn is_canonical_unsigned_digits(s: &str) -> bool {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    s == "0" || !s.starts_with('0')
}

/// True if `s` is the canonical decimal rendering of some `i64`: no leading
/// `+`, no leading zeros, and no `-0` — canonical zero is `"0"`.
pub(crate) fn is_canonical_signed_decimal(s: &str) -> bool {
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
