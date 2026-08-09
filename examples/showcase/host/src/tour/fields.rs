//! The host's typed view of the store.
//!
//! Rust gets the same four-way [`Reading`] the guests do, and the same
//! "the codec refused it" arm, through [`TypedStore::field`]. What Rust does
//! *not* get is a generated accessor tree: `duet generate` emits Dart and
//! TypeScript, so the paths below are written out as literals here while the two
//! guests read `state.document.lines`. They are `&'static str` by construction,
//! validated once when the field is built rather than on every read — but they
//! are still hand-written, and that asymmetry is recorded in the README.

use duet::{Field, PathParseError, TypedStore};

use duet_showcase::state::{HostNote, Presence};

/// Every field the tour reads or writes.
pub struct Fields {
    /// `document.title`.
    pub title: Field<String>,
    /// `document.lines`.
    pub lines: Field<Vec<String>>,
    /// `host`, written whole so a guest sees one consistent commentary.
    pub host: Field<HostNote>,
    /// The Flutter guest's subtree.
    pub flutter: GuestFields,
    /// The webview guest's subtree.
    pub web: GuestFields,
}

/// One guest's subtree.
pub struct GuestFields {
    /// The whole `Presence`, for printing.
    pub all: Field<Presence>,
    /// `<guest>.status`.
    pub status: Field<String>,
    /// `<guest>.note` — what this guest wrote for its peer to read.
    pub note: Field<String>,
    /// `<guest>.saw_peer_note`.
    pub saw_peer_note: Field<String>,
    /// `<guest>.saw_lines`.
    pub saw_lines: Field<i64>,
    /// `<guest>.returned`.
    pub returned: Field<String>,
    /// `<guest>.raised`.
    pub raised: Field<String>,
}

impl Fields {
    /// Builds every handle up front, so a mistyped path is one startup failure
    /// rather than a surprise twelve acts in.
    ///
    /// # Errors
    ///
    /// Returns the first [`PathParseError`] if any literal above is not a path.
    pub fn bind(store: &TypedStore) -> Result<Fields, PathParseError> {
        Ok(Fields {
            title: store.field("document.title")?,
            lines: store.field("document.lines")?,
            host: store.field("host")?,
            flutter: GuestFields {
                all: store.field("flutter")?,
                status: store.field("flutter.status")?,
                note: store.field("flutter.note")?,
                saw_peer_note: store.field("flutter.saw_peer_note")?,
                saw_lines: store.field("flutter.saw_lines")?,
                returned: store.field("flutter.returned")?,
                raised: store.field("flutter.raised")?,
            },
            web: GuestFields {
                all: store.field("web")?,
                status: store.field("web.status")?,
                note: store.field("web.note")?,
                saw_peer_note: store.field("web.saw_peer_note")?,
                saw_lines: store.field("web.saw_lines")?,
                returned: store.field("web.returned")?,
                raised: store.field("web.raised")?,
            },
        })
    }
}

impl GuestFields {
    /// Wipes everything this guest published.
    ///
    /// Called on the guest's behalf, after it is gone: a torn-down guest cannot
    /// retract its own claims, and leaving them in place would make the reboot
    /// unfalsifiable — the values a rebooted guest is supposed to rediscover
    /// would already be sitting there.
    pub fn clear(&self, status: &str) -> Result<(), String> {
        let set = |what: &str, outcome: Result<(), duet::FieldError>| {
            outcome.map_err(|e| format!("clearing {what} failed: {e}"))
        };
        set("status", self.status.set(&status.to_string()))?;
        set("saw_peer_note", self.saw_peer_note.set(&String::new()))?;
        set("saw_lines", self.saw_lines.set(&0))?;
        set("returned", self.returned.set(&String::new()))?;
        set("raised", self.raised.set(&String::new()))?;
        Ok(())
    }
}

/// Reads a `String` field, rendering every arm of the reading.
///
/// A read is never an exception here either: `absent` and `mismatch` are states
/// the report has to be able to print, not errors to propagate.
pub fn text(field: &Field<String>) -> String {
    match field.get() {
        Ok(duet::Reading::Present(value)) => value,
        Ok(duet::Reading::None) => "(null)".to_string(),
        Ok(duet::Reading::Absent) => "(absent)".to_string(),
        Ok(duet::Reading::Mismatch { found, .. }) => format!("(mismatch: {found:?})"),
        Err(e) => format!("(unreadable: {e})"),
    }
}

/// Reads an `i64` field, or `None` if it is not readable as one.
pub fn int(field: &Field<i64>) -> Option<i64> {
    match field.get() {
        Ok(duet::Reading::Present(value)) => Some(value),
        _ => None,
    }
}

/// Reads the document's lines, or an empty list if they are not readable.
pub fn lines(field: &Field<Vec<String>>) -> Vec<String> {
    match field.get() {
        Ok(duet::Reading::Present(value)) => value,
        _ => Vec::new(),
    }
}
