//! The one definition everything else in the showcase is generated from.
//!
//! These structs are the *only* place any field of the shared store is spelled
//! out. `#[derive(SharedState)]` turns them into a schema document
//! (`examples/showcase/schema/showcase.json`), and `duet generate` turns that
//! document into the Dart and TypeScript clients both guests use. Neither guest
//! ever writes a path string of its own; they call `showcase.document.title`
//! and the generated accessor carries the literal `"document.title"` that was
//! minted and validated when the client was generated.
//!
//! Wire keys are the Rust field names, unchanged. Accessor names in the guests
//! are camel-cased from them — `saw_peer_note` here is `sawPeerNote` in Dart and
//! TypeScript, addressing the same `flutter.saw_peer_note` key. That asymmetry
//! is deliberate, and it is why guests must not hand-write paths.

use duet::SharedState;

/// The whole shared store.
///
/// One `Presence` per guest rather than one shared "presence" node: each guest
/// owns exactly one subtree and watches the other's, so "the peer's write is
/// visible" is a claim about a value this guest never wrote.
#[derive(Debug, Clone, PartialEq, Eq, SharedState)]
pub struct Showcase {
    /// The document both guests compose into.
    pub document: Document,
    /// What the Flutter guest is doing and what it has seen.
    pub flutter: Presence,
    /// What the webview guest is doing and what it has seen.
    pub web: Presence,
    /// What the Rust host is doing, so a guest's UI can narrate along.
    pub host: HostNote,
}

/// The shared document.
#[derive(Debug, Clone, PartialEq, Eq, SharedState)]
pub struct Document {
    /// A title the host owns and both guests display.
    pub title: String,
    /// The composed lines. Only [`append_line`] ever writes this, so both
    /// guests' contributions land in one list without either overwriting the
    /// other.
    ///
    /// [`append_line`]: crate::commands::append_line
    pub lines: Vec<String>,
}

/// One guest's view of the world, published for the host and the other guest
/// to read.
///
/// Every field is a string or an integer rather than a richer shape because
/// this subtree is *evidence*: the host prints it verbatim, and a value that
/// reads the same in the terminal as it does in the store is one fewer place
/// for a demo to be quietly wrong.
#[derive(Debug, Clone, PartialEq, Eq, SharedState)]
pub struct Presence {
    /// `"booting"`, `"ready"`, or `"torn down"` — the last written by the host,
    /// not the guest, because a torn-down guest cannot say so itself.
    pub status: String,
    /// A greeting this guest wrote for its peer to read.
    pub note: String,
    /// What this guest's watcher saw in the *peer's* `note`. Empty until the
    /// peer has written and the push has arrived.
    pub saw_peer_note: String,
    /// How many lines this guest's watcher last saw in `document.lines`.
    /// Updated from a push, never from a poll.
    pub saw_lines: i64,
    /// The `returned` arm of this guest's command call, rendered for humans.
    pub returned: String,
    /// The `raised` arm of this guest's command call, rendered for humans.
    pub raised: String,
}

/// The host's running commentary, published into the store.
#[derive(Debug, Clone, PartialEq, Eq, SharedState)]
pub struct HostNote {
    /// The act the tour is currently in.
    pub act: String,
    /// One line of detail about it.
    pub detail: String,
}

/// The domain error [`append_line`] raises.
///
/// A `raises` type is reachable from the schema only through a command, which
/// is what makes it worth having here: it proves the schema's `types` list
/// carries types the *state* never mentions.
///
/// [`append_line`]: crate::commands::append_line
#[derive(Debug, Clone, PartialEq, Eq, SharedState)]
pub struct ComposeError {
    /// A stable, machine-readable tag: `"empty_line"`, `"line_too_long"`,
    /// `"store"`.
    pub code: String,
    /// A human-readable explanation.
    pub detail: String,
}

impl ComposeError {
    /// Builds an error with a stable code and a human-readable detail.
    pub fn new(code: &str, detail: impl Into<String>) -> Self {
        ComposeError {
            code: code.to_string(),
            detail: detail.into(),
        }
    }
}

impl Presence {
    /// The state a guest is in before it has said anything.
    pub fn booting() -> Self {
        Presence {
            status: "booting".to_string(),
            note: String::new(),
            saw_peer_note: String::new(),
            saw_lines: 0,
            returned: String::new(),
            raised: String::new(),
        }
    }
}

/// The store the host installs at startup.
///
/// Every field is materialized, including the empty strings. `Value::set` never
/// creates intermediate nodes, so a guest could not bring `flutter` or
/// `document` into existence on its own — seeding the whole tree is what makes
/// every generated accessor writable from the first turn.
pub fn initial_state() -> Showcase {
    Showcase {
        document: Document {
            title: "untitled".to_string(),
            lines: Vec::new(),
        },
        flutter: Presence::booting(),
        web: Presence::booting(),
        host: HostNote {
            act: "starting".to_string(),
            detail: "the host has not opened a surface yet".to_string(),
        },
    }
}
