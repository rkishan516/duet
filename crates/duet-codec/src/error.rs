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
