//! Why a session could not be started.

use std::fmt;

/// Why [`crate::Session::open`] refused.
///
/// Every variant is a *startup* failure. Once a session exists nothing can fail
/// it: serving a line is total, and a store request can only fail if the core
/// thread is gone, which this crate's own `Drop` is the only thing that does.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SessionError {
    /// No embedded schema fixture goes by that name.
    UnknownFixture {
        /// The name that was asked for, bounded so a hostile argv cannot turn
        /// a usage message into an unbounded one.
        asked: String,
        /// Every name that would have worked.
        known: Vec<&'static str>,
    },
    /// An embedded schema fixture no longer parses.
    ///
    /// Unreachable while `schema/*.json` and `duet-codegen`'s reader agree —
    /// the schemas are compiled into this binary with `include_str!`, so a
    /// change to either rebuilds it. Present so the failure names the fixture
    /// rather than arriving as a panic in a process a guest is waiting on.
    UnreadableFixture {
        /// The fixture that failed to parse.
        name: &'static str,
        /// What the reader said.
        because: String,
    },
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SessionError::UnknownFixture { asked, known } => write!(
                f,
                "no schema fixture named \"{asked}\"; this host knows {}",
                known.join(", ")
            ),
            SessionError::UnreadableFixture { name, because } => {
                write!(
                    f,
                    "the embedded schema fixture \"{name}\" is unreadable: {because}"
                )
            }
        }
    }
}

impl std::error::Error for SessionError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unknown_fixture_names_what_would_have_worked() {
        // A usage failure that does not list the alternatives sends a
        // developer to the source. Naming them is the whole cost of not doing
        // that.
        let error = SessionError::UnknownFixture {
            asked: "nope".to_string(),
            known: vec!["app", "wide"],
        };
        assert_eq!(
            error.to_string(),
            "no schema fixture named \"nope\"; this host knows app, wide"
        );
    }

    #[test]
    fn an_unreadable_fixture_names_the_fixture_and_the_reason() {
        let error = SessionError::UnreadableFixture {
            name: "app",
            because: "expected an object".to_string(),
        };
        assert_eq!(
            error.to_string(),
            "the embedded schema fixture \"app\" is unreadable: expected an object"
        );
    }
}
