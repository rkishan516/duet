//! Bounded rendering of text a guest process chose.

use std::fmt::Write as _;

use duet_core::Segment;

/// The most **characters** of guest-supplied text any message here renders.
///
/// Characters, never bytes: slicing a `&str` on a byte index that lands inside
/// a multi-byte character panics, and a guest picks the characters. A guest
/// that writes a map key of a thousand emoji must not be able to abort the
/// host through an error message.
pub(crate) const MAX_ECHO_CHARS: usize = 64;

/// Renders `text` capped at [`MAX_ECHO_CHARS`], appending `…` when it was cut.
pub(crate) fn truncate(text: &str) -> String {
    let mut out: String = text.chars().take(MAX_ECHO_CHARS).collect();
    if text.chars().nth(MAX_ECHO_CHARS).is_some() {
        out.push('…');
    }
    out
}

/// Renders a location *inside* a decoded value, bounded.
///
/// The segments come from two very different places and only one of them is
/// trusted: a struct field key is a `&'static str` the host wrote, while a
/// `HashMap<String, V>` key is whatever a guest put in the store. Bounding the
/// whole rendering rather than only the untrusted half keeps that distinction
/// from having to be tracked segment by segment, and costs nothing — a decode
/// error deep inside a 10 000-element list is no more actionable at full
/// length than truncated.
///
/// The empty location renders as `<root>` rather than as an empty string, so a
/// message never trails off into nothing.
pub(crate) fn location(segments: &[Segment]) -> String {
    if segments.is_empty() {
        return "<root>".to_string();
    }
    let mut out = String::new();
    let mut first = true;
    for segment in segments {
        match segment {
            Segment::Key(key) => {
                if !first {
                    out.push('.');
                }
                out.push_str(key);
            }
            // `write!` to a `String` cannot fail; the `Result` is discarded
            // rather than unwrapped so this file keeps its no-panic property.
            Segment::Index(index) => {
                let _ = write!(out, "[{index}]");
            }
        }
        first = false;
    }
    truncate(&out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_is_untouched() {
        assert_eq!(truncate("editor.zoom"), "editor.zoom");
    }

    #[test]
    fn long_text_is_capped_and_marked() {
        let rendered = truncate(&"k".repeat(1_000));
        assert_eq!(rendered.chars().count(), MAX_ECHO_CHARS + 1);
        assert!(rendered.ends_with('…'));
    }

    #[test]
    fn truncation_never_splits_a_multibyte_character() {
        // A guest picks these characters. Byte-slicing would abort the host.
        let rendered = truncate(&"\u{1F600}".repeat(1_000));
        assert!(rendered.starts_with('\u{1F600}'));
        assert_eq!(rendered.chars().count(), MAX_ECHO_CHARS + 1);
    }

    #[test]
    fn an_empty_location_renders_as_root() {
        assert_eq!(location(&[]), "<root>");
    }

    #[test]
    fn a_location_mixes_keys_and_indices() {
        assert_eq!(
            location(&[
                Segment::Key("documents".to_string()),
                Segment::Index(3),
                Segment::Key("title".to_string()),
            ]),
            "documents[3].title"
        );
    }

    #[test]
    fn a_leading_index_needs_no_separator() {
        assert_eq!(location(&[Segment::Index(0)]), "[0]");
    }

    #[test]
    fn a_guest_chosen_map_key_is_bounded_in_a_location() {
        let rendered = location(&[Segment::Key("k".repeat(10_000))]);
        assert_eq!(rendered.chars().count(), MAX_ECHO_CHARS + 1);
    }
}
