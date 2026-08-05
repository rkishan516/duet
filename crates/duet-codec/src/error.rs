//! Errors produced when encoding or decoding the wire format.

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

impl CodecError {
    /// A short, stable, machine-readable name for *why* a payload was refused.
    ///
    /// Unlike [`Display`](std::fmt::Display), which is prose for a developer and
    /// free to change, these strings are part of the cross-language contract:
    /// the golden wire corpus at `corpus/wire-corpus.json` records one per
    /// reject case, so the Rust, Dart and JavaScript decoders must agree not
    /// merely that a message is bad but on *which rule* it broke.
    ///
    /// # Why this lives in `duet-codec` and not in the consumer that needs it
    ///
    /// [`CodecError`] is `#[non_exhaustive]`, so the same match written in any
    /// other crate would need a wildcard arm. That arm would silently hand a
    /// future variant some existing — and therefore wrong — code, which is
    /// precisely the "rejected, but for the wrong reason" failure the corpus
    /// exists to catch. Inside the defining crate the match is exhaustive and
    /// adding a variant without a code is a compile error.
    ///
    /// # `bad_json` has no variant here
    ///
    /// The corpus uses one further reason, `bad_json`, for input that fails
    /// `serde_json` parsing outright — unbalanced braces, or nesting past
    /// serde_json's own recursion limit. That text never reaches this crate:
    /// the JSON parser rejects it before any `CodecError` could be constructed,
    /// so `bad_json` is deliberately absent from this enum rather than an
    /// omission.
    pub fn reason_code(&self) -> &'static str {
        match self {
            CodecError::UnknownTag(_) => "unknown_tag",
            CodecError::BadShape(_) => "bad_shape",
            CodecError::BadInt(_) => "bad_int",
            CodecError::BadFloat(_) => "bad_float",
            CodecError::BadBase64(_) => "bad_base64",
            CodecError::BadPath(_) => "bad_path",
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bad_shape_displays_actionably() {
        let rendered = CodecError::BadShape("missing \"t\"".to_string()).to_string();
        assert!(rendered.contains("\"t\""), "got: {rendered}");
    }

    #[test]
    fn unknown_tag_names_the_tag() {
        let rendered = CodecError::UnknownTag("q".to_string()).to_string();
        assert!(rendered.contains('q'), "got: {rendered}");
    }

    #[test]
    fn guest_supplied_text_is_bounded_in_messages() {
        // This crate parses untrusted guest input. An unbounded echo means a
        // 1 MB tag produces a 1 MB log line.
        let huge = "z".repeat(10_000);
        let source = CodecError::UnknownTag(huge.clone());
        let rendered = source.to_string();
        assert!(rendered.len() < 200, "got {} chars", rendered.len());

        // The struct field itself must keep the full value for Debug and
        // tests, even though Display truncates it. `if let` without an else
        // arm keeps this total: there is no failure branch to leave dead.
        let mut field_len = None;
        if let CodecError::UnknownTag(t) = &source {
            field_len = Some(t.len());
        }
        assert_eq!(field_len, Some(10_000));
    }

    #[test]
    fn codec_error_is_a_std_error() {
        fn assert_error<E: std::error::Error>() {}
        assert_error::<CodecError>();
    }

    #[test]
    fn every_error_variant_has_a_distinct_reason_code() {
        let all = [
            CodecError::UnknownTag(String::new()),
            CodecError::BadShape(String::new()),
            CodecError::BadInt(String::new()),
            CodecError::BadFloat(String::new()),
            CodecError::BadBase64(String::new()),
            CodecError::BadPath(String::new()),
        ];
        let codes: Vec<&str> = all.iter().map(CodecError::reason_code).collect();
        let unique: std::collections::BTreeSet<&&str> = codes.iter().collect();
        assert_eq!(
            unique.len(),
            codes.len(),
            "reason codes must be distinct: {codes:?}"
        );
        // Lowercase snake_case, digits allowed: `bad_base64` carries "64" in
        // its name. A letters-and-underscore-only predicate would reject it —
        // the point of the check is that these are stable identifiers safe to
        // paste into a Dart or TypeScript switch, not that they are alphabetic.
        assert!(
            codes.iter().all(|c| c
                .chars()
                .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')),
            "reason codes must be lowercase snake_case: {codes:?}"
        );
    }

    #[test]
    fn reason_codes_are_pinned_exactly() {
        // Distinctness alone would let a rename through, and these strings are
        // a cross-language contract: `corpus/wire-corpus.json` stores them, and
        // Dart and TypeScript tests will compare against them literally. A
        // rename must be a deliberate corpus regeneration, not a silent drift.
        assert_eq!(
            CodecError::UnknownTag(String::new()).reason_code(),
            "unknown_tag"
        );
        assert_eq!(
            CodecError::BadShape(String::new()).reason_code(),
            "bad_shape"
        );
        assert_eq!(CodecError::BadInt(String::new()).reason_code(), "bad_int");
        assert_eq!(
            CodecError::BadFloat(String::new()).reason_code(),
            "bad_float"
        );
        assert_eq!(
            CodecError::BadBase64(String::new()).reason_code(),
            "bad_base64"
        );
        assert_eq!(CodecError::BadPath(String::new()).reason_code(), "bad_path");
    }

    #[test]
    fn the_reason_code_ignores_the_payload_text() {
        // The code names the rule that was broken, not the offending input, so
        // two failures of the same rule must report the same code however
        // different their messages are.
        assert_eq!(
            CodecError::BadInt("007".to_string()).reason_code(),
            CodecError::BadInt("payload must be a decimal string".to_string()).reason_code()
        );
    }

    #[test]
    fn every_variant_names_its_kind_in_display() {
        // Each match arm in Display renders a distinct, identifiable prefix.
        // Pinning all six here (bad_shape_displays_actionably and
        // unknown_tag_names_the_tag above cover BadShape and UnknownTag with
        // more targeted assertions) closes the remaining four.
        assert_eq!(
            CodecError::BadInt("nope".to_string()).to_string(),
            "invalid integer payload \"nope\""
        );
        assert_eq!(
            CodecError::BadFloat("huge".to_string()).to_string(),
            "invalid float payload \"huge\""
        );
        assert_eq!(
            CodecError::BadBase64("!!!".to_string()).to_string(),
            "invalid base64 payload: !!!"
        );
        assert_eq!(
            CodecError::BadPath("foo]".to_string()).to_string(),
            "invalid path: foo]"
        );
    }
}
