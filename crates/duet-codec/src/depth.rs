//! The Duet wire's nesting limit, and the iterative scan that enforces it.
//!
//! Nesting is the one wire rule that cannot be checked *after* parsing: by the
//! time a recursive-descent parser has discovered how deep a document goes, it
//! has already recursed that far. So the limit is stated here as a number the
//! format owns, and enforced by a scan over the raw text that runs *before* any
//! parser sees it.

/// The most nested JSON containers a Duet message may contain: **127**.
///
/// A "container" is one `[` or one `{`. `[[1]]` is two; `{"t":"l","v":[]}` is
/// two (the tagged object and its array), which is why a `duet_core::Value`
/// costs two containers per level of its own nesting rather than one.
///
/// # All three implementations must agree on this number exactly
///
/// Rust, Dart and TypeScript each ship a decoder for this format, and
/// `corpus/wire-corpus.json` pins the boundary in all three at once
/// (`value/nesting/at_limit` accepts, `value/nesting/over_limit` rejects). A
/// guest whose limit is one higher than the host's *accepts a document the host
/// refuses* — a divergence that no "it rejects deep input" test can see, because
/// a 200-level test case passes in every implementation whatever the off-by-one
/// is. Only a test pinned at exactly 127 and exactly 128 can.
///
/// That is not hypothetical: the Dart and TypeScript clients shipped with 128
/// here and this project measured the resulting one-level divergence.
///
/// # Why Duet owns the number rather than inheriting it
///
/// 127 is what `serde_json` — the host's parser — actually accepts; it refuses
/// the 128th container. But that is a constant *inside a dependency*, not a
/// promise it makes. If a future `serde_json` raised it, the host would silently
/// start accepting documents the two guests still reject, re-opening the
/// divergence from the other side and with nothing failing to say so.
///
/// So the host enforces this bound itself, with [`exceeds_max_json_depth`],
/// before handing text to any parser. `serde_json`'s own limit then becomes a
/// second line of defence that this crate does not depend on, and
/// `crates/duet-codec/tests/round_trip.rs` pins the two against each other so a
/// dependency bump that moves them apart fails loudly.
pub const MAX_JSON_DEPTH: usize = 127;

/// True if `text` nests JSON containers deeper than [`MAX_JSON_DEPTH`].
///
/// A **pre-scan**: it reads raw wire text, so a caller runs it *before*
/// `serde_json::from_str` rather than inspecting a tree that parsing already had
/// to build. `duet_protocol::handle_text` is the caller that matters — it sits
/// on the guest trust boundary.
///
/// # Iterative, and it has to be
///
/// A recursive depth check dies by stack overflow on exactly the input it exists
/// to reject, and a stack overflow in Rust is an abort, not a catchable error.
/// This walks the bytes with a single counter: no recursion, no allocation, one
/// pass, and it stops early once the limit is exceeded.
///
/// # It is a *bound*, not a parser
///
/// Malformed text is not this function's problem — it only answers "is anything
/// in here nested too deeply", and the parser behind it rejects everything else.
/// It does track JSON string literals and their escapes, because it must:
/// without that, the perfectly legal one-container document `["[[[…"]` would be
/// counted as thousands and refused. Unbalanced closers saturate at zero rather
/// than underflowing, so `]]]]{` reports the depth of its `{` and not garbage.
///
/// Scanning bytes rather than `char`s is safe here and deliberate: every byte of
/// a multi-byte UTF-8 sequence is `>= 0x80`, so none can be mistaken for one of
/// the ASCII delimiters below, and a byte scan costs nothing per non-ASCII
/// character.
pub fn exceeds_max_json_depth(text: &str) -> bool {
    let mut depth: usize = 0;
    let mut in_string = false;
    let mut escaped = false;
    for byte in text.bytes() {
        if in_string {
            if escaped {
                // Whatever follows a backslash is literal, including `"` and a
                // second backslash. `\uXXXX`'s four hex digits need no special
                // handling: none of them is a delimiter.
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'[' | b'{' => {
                depth += 1;
                if depth > MAX_JSON_DEPTH {
                    return true;
                }
            }
            b']' | b'}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `depth` nested arrays around a scalar: exactly `depth` containers.
    fn arrays(depth: usize) -> String {
        format!("{}1{}", "[".repeat(depth), "]".repeat(depth))
    }

    /// `depth` nested objects around a scalar: exactly `depth` containers.
    fn objects(depth: usize) -> String {
        let mut text = "1".to_string();
        for _ in 0..depth {
            text = format!("{{\"a\":{text}}}");
        }
        text
    }

    #[test]
    fn the_limit_is_pinned_at_the_exact_boundary() {
        // THE test this module exists for. A case at 200 levels would pass with
        // an off-by-one still present — which is exactly how a one-level
        // divergence between the host and both guests survived until it was
        // measured. Only 127-accept plus 128-reject can see it.
        for shape in [arrays as fn(usize) -> String, objects] {
            assert!(
                !exceeds_max_json_depth(&shape(MAX_JSON_DEPTH)),
                "exactly {MAX_JSON_DEPTH} containers must be accepted"
            );
            assert!(
                exceeds_max_json_depth(&shape(MAX_JSON_DEPTH + 1)),
                "exactly {} containers must be refused",
                MAX_JSON_DEPTH + 1
            );
        }
    }

    #[test]
    fn the_limit_is_127_because_that_is_what_the_host_parser_accepts() {
        // Stated as a literal as well as measured against serde_json in
        // `tests/round_trip.rs`, so that reading this constant's value never
        // requires running a dependency to find out what it is. The Dart and
        // TypeScript mirrors of this number are pinned the same way.
        assert_eq!(MAX_JSON_DEPTH, 127);
    }

    #[test]
    fn nesting_that_alternates_shapes_counts_every_container() {
        // A counter that only tracked one delimiter kind would pass the two
        // uniform cases above and still be wrong for real Duet text, which
        // alternates objects and arrays at every level.
        let mut text = "1".to_string();
        for i in 0..MAX_JSON_DEPTH {
            text = if i % 2 == 0 {
                format!("[{text}]")
            } else {
                format!("{{\"a\":{text}}}")
            };
        }
        assert!(!exceeds_max_json_depth(&text));
        assert!(exceeds_max_json_depth(&format!("[{text}]")));
    }

    #[test]
    fn siblings_are_not_cumulative() {
        // Depth, not a count: a million shallow containers side by side is a
        // perfectly ordinary payload and must not be refused.
        let flat = format!("[{}]", vec!["[1]"; 100_000].join(","));
        assert!(!exceeds_max_json_depth(&flat));
    }

    #[test]
    fn brackets_inside_string_literals_are_not_containers() {
        // Without string tracking this document — one container holding one
        // string — would be counted as 200 and refused, breaking every guest
        // that ever sends a bracket in a path or a `Str` payload.
        let inner = "[".repeat(200);
        assert!(!exceeds_max_json_depth(&format!("[\"{inner}\"]")));
        // An escaped quote must not be mistaken for the end of the string, or
        // everything after it is counted as structure.
        assert!(!exceeds_max_json_depth(&format!("[\"\\\"{inner}\"]")));
        // A trailing escaped backslash ends the string on the NEXT quote, not
        // on the one the backslash escapes.
        assert!(!exceeds_max_json_depth("[\"a\\\\\",\"b\"]"));
    }

    #[test]
    fn unbalanced_closers_do_not_underflow() {
        // Malformed input reaches this scan — it runs before any parser. A
        // naive `depth -= 1` would wrap to usize::MAX here and report every
        // document that follows as over-deep.
        assert!(!exceeds_max_json_depth("]]]]]]"));
        assert!(!exceeds_max_json_depth(&format!(
            "]]]]{}",
            arrays(MAX_JSON_DEPTH)
        )));
        assert!(exceeds_max_json_depth(&format!(
            "]]]]{}",
            arrays(MAX_JSON_DEPTH + 1)
        )));
    }

    #[test]
    fn the_scan_survives_input_that_would_overflow_a_recursive_one() {
        // The whole reason this is a loop and not a recursion: 500 000 unclosed
        // brackets is a stack overflow — an abort, not an error — for any
        // recursive scan, on precisely the input the scan exists to reject.
        assert!(exceeds_max_json_depth(&"[".repeat(500_000)));
    }

    #[test]
    fn non_ascii_text_is_not_miscounted() {
        // The scan reads bytes, not chars. Every byte of a multi-byte UTF-8
        // sequence is >= 0x80, so none can collide with a delimiter — but that
        // is a claim worth pinning rather than asserting in a comment.
        assert!(!exceeds_max_json_depth("[\"café ✓ 😀\"]"));
        assert!(!exceeds_max_json_depth(&format!(
            "[\"{}\"]",
            "😀".repeat(1_000)
        )));
    }

    #[test]
    fn an_empty_or_scalar_document_nests_nothing() {
        for text in ["", "null", "42", "\"a\"", "true"] {
            assert!(!exceeds_max_json_depth(text), "{text:?} nests nothing");
        }
    }
}
