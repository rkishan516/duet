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
        assert!(
            rendered.contains("\"t\""),
            "should surface the detail, got: {rendered}"
        );
    }

    #[test]
    fn unknown_tag_names_the_tag() {
        let rendered = CodecError::UnknownTag("q".to_string()).to_string();
        assert!(
            rendered.contains('q'),
            "should name the offending tag, got: {rendered}"
        );
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
