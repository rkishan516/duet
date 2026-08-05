//! Bounding guest-supplied text that gets echoed back into error messages.
//!
//! Paths arrive from guest processes (a Flutter surface, a webview that may be
//! running untrusted web content) and are carried verbatim inside
//! [`SetError`](crate::value::SetError) so a guest can locate the problem. That
//! makes every error `Display` an amplifier: a guest that sends a 1 MB path and
//! gets a 1 MB error string back has turned one request into a 1 MB log line
//! *and* a 1 MB protocol reply. The cost scales linearly with input the host
//! does not control, which is a denial-of-service primitive, not a formatting
//! nit.
//!
//! `duet-codec` solves the same problem for the text *it* decodes, with the
//! same 48-character cap. This module is the `duet-core` equivalent; it is
//! written by hand rather than shared, because `duet-core` is deliberately
//! zero-dependency and must not take one on `duet-codec` (which sits above it)
//! to get four lines of string handling.
//!
//! Truncation is by **character**, never by byte. `&str[..n]` panics when `n`
//! lands inside a multi-byte UTF-8 sequence, and paths are guest-chosen, so a
//! byte-indexed cap would let a guest of emoji convert a log-bloat bug into a
//! host crash — strictly worse than the bug being fixed.
//!
//! Only `Display` is bounded. The error's own fields keep the full path, so
//! `Debug`, tests, and any host-side handler that genuinely needs the whole
//! thing still have it.

use std::fmt::{Display, Write};

/// How many characters of guest-supplied text to include in a `Display`
/// message.
///
/// Matches `duet_codec`'s `MAX_ECHO_CHARS`. Long enough that a realistic path
/// (`documents[3].title`) is shown whole, short enough that no guest input can
/// make an error message meaningfully large.
const MAX_ECHO_CHARS: usize = 48;

/// The character appended when text was dropped, so a reader can tell a
/// truncated path from a short one.
const ELLIPSIS: char = '…';

/// Renders `value` and caps it at [`MAX_ECHO_CHARS`] characters, appending
/// [`ELLIPSIS`] if anything was dropped.
///
/// Never allocates more than the cap: the rendered text is discarded as it is
/// produced rather than being buffered whole and then trimmed, so formatting a
/// 1 MB path does not transiently cost 1 MB.
pub(crate) fn truncated(value: &impl Display) -> String {
    let mut writer = Truncating::new(MAX_ECHO_CHARS);
    // `Truncating::write_str` is infallible, so this cannot fail. Even if a
    // future `Display` implementation returned `Err`, a partial rendering is a
    // better outcome here than refusing to render the error at all — this
    // function has no error path of its own to report it through.
    let _ = write!(writer, "{value}");
    writer.finish()
}

/// A [`Write`] sink that keeps the first `remaining` characters and discards
/// the rest, remembering that it did so.
struct Truncating {
    /// The characters kept so far.
    shown: String,
    /// How many more characters may still be kept.
    remaining: usize,
    /// Whether at least one character was discarded.
    dropped_any: bool,
}

impl Truncating {
    fn new(limit: usize) -> Self {
        Truncating {
            shown: String::new(),
            remaining: limit,
            dropped_any: false,
        }
    }

    /// Consumes the sink, returning the kept text plus an ellipsis if anything
    /// was discarded.
    fn finish(mut self) -> String {
        if self.dropped_any {
            self.shown.push(ELLIPSIS);
        }
        self.shown
    }
}

impl Write for Truncating {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        // Iterating by `char` is what keeps the cut on a character boundary.
        // Returning early once the budget is spent is what keeps this O(cap)
        // rather than O(len) per call, so a huge path is never walked in full.
        for ch in s.chars() {
            if self.remaining == 0 {
                self.dropped_any = true;
                return Ok(());
            }
            self.shown.push(ch);
            self.remaining -= 1;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_passes_through_untouched_with_no_ellipsis() {
        assert_eq!(truncated(&"documents[3].title"), "documents[3].title");
    }

    #[test]
    fn text_exactly_at_the_cap_is_not_marked_as_truncated() {
        // The boundary that an off-by-one would get wrong: this must not gain
        // an ellipsis, and the next test's one-longer input must.
        let exact = "a".repeat(MAX_ECHO_CHARS);
        assert_eq!(truncated(&exact), exact);
    }

    #[test]
    fn one_character_over_the_cap_is_truncated_and_marked() {
        let over = "a".repeat(MAX_ECHO_CHARS + 1);
        let rendered = truncated(&over);
        assert_eq!(rendered.chars().count(), MAX_ECHO_CHARS + 1);
        assert!(rendered.ends_with(ELLIPSIS));
        assert_eq!(
            rendered.chars().filter(|c| *c == 'a').count(),
            MAX_ECHO_CHARS
        );
    }

    #[test]
    fn empty_text_stays_empty() {
        assert_eq!(truncated(&""), "");
    }

    #[test]
    fn a_huge_input_produces_a_bounded_output() {
        let rendered = truncated(&"z".repeat(1_000_000));
        assert_eq!(rendered.chars().count(), MAX_ECHO_CHARS + 1);
    }

    #[test]
    fn multibyte_characters_are_never_split() {
        // Every kept character must survive intact. `String` cannot hold a
        // partial `char` at all, so the real hazard this guards is a future
        // rewrite reaching for byte slicing; the assertion pins the observable
        // property either way.
        let rendered = truncated(&"\u{1F600}".repeat(1_000));
        assert_eq!(
            rendered.chars().filter(|c| *c == '\u{1F600}').count(),
            MAX_ECHO_CHARS
        );
        assert!(rendered.ends_with(ELLIPSIS));
    }

    #[test]
    fn the_cap_counts_characters_not_bytes() {
        // 48 four-byte characters are 192 bytes. A byte-counting cap would
        // keep only ~12 of them, so this distinguishes the two.
        let rendered = truncated(&"\u{1F600}".repeat(100));
        assert_eq!(rendered.chars().count(), MAX_ECHO_CHARS + 1);
        assert!(
            rendered.len() > MAX_ECHO_CHARS,
            "got {} bytes",
            rendered.len()
        );
    }

    #[test]
    fn text_arriving_in_many_small_writes_is_capped_the_same_way() {
        // `Display` implementations write incrementally — `Path` emits one
        // `write!` per segment — so the budget must persist across calls
        // rather than resetting on each one.
        struct ManyWrites;
        impl Display for ManyWrites {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                for _ in 0..1_000 {
                    write!(f, "ab")?;
                }
                Ok(())
            }
        }
        let rendered = truncated(&ManyWrites);
        assert_eq!(rendered.chars().count(), MAX_ECHO_CHARS + 1);
    }
}
