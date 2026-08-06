//! Why a typed write, or an installation, did not happen.

use duet_core::Path;
use duet_runtime::RuntimeError;

use crate::error::SchemaErrors;

/// Why a typed write was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum FieldError {
    /// The value would leave the store nested past
    /// [`MAX_VALUE_DEPTH`](duet_core::MAX_VALUE_DEPTH).
    ///
    /// Reported by the typed layer before the write is sent, so the caller sees
    /// a named condition rather than a store error arriving from another
    /// thread. Such a store could no longer encode itself: a read of it would
    /// produce text past the wire's nesting limit, which no conforming client
    /// — the host's own decoder included — can parse.
    TooDeep {
        /// The field's path.
        path: Path,
        /// The nesting the store would have had: the path's segment count plus
        /// the written value's own depth.
        depth: usize,
        /// The most it may have.
        max: usize,
    },
    /// The store refused the write, or could not be reached.
    ///
    /// The commonest cause is not a transport problem at all: the store never
    /// creates intermediate nodes, so writing to `editor.zoom` when `editor` is
    /// `Value::Null` or absent fails, carrying
    /// [`SetError`](duet_core::SetError) inside.
    Store(RuntimeError),
}

impl std::fmt::Display for FieldError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // The path here is host-authored — a `&'static str` literal that
            // already passed the schema's key checks — so it is not bounded the
            // way a guest-supplied path is.
            FieldError::TooDeep { path, depth, max } => write!(
                f,
                "the value at path \"{path}\" would nest {depth} containers, \
                 past the limit of {max}"
            ),
            FieldError::Store(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for FieldError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            FieldError::TooDeep { .. } => None,
            FieldError::Store(error) => Some(error),
        }
    }
}

impl From<RuntimeError> for FieldError {
    fn from(error: RuntimeError) -> FieldError {
        FieldError::Store(error)
    }
}

/// Why a typed root could not be installed into a store.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum InstallError {
    /// The root type's schema is not valid.
    ///
    /// Checked before anything is written, because a schema with a cycle, a
    /// colliding name or an illegal key would generate clients that cannot
    /// address the store — a startup bug worth failing on rather than
    /// discovering when a guest connects.
    Schema(SchemaErrors),
    /// The store refused the seed write, or could not be reached.
    Store(RuntimeError),
}

impl std::fmt::Display for InstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InstallError::Schema(errors) => write!(f, "invalid schema: {errors}"),
            InstallError::Store(error) => write!(f, "could not seed the store: {error}"),
        }
    }
}

impl std::error::Error for InstallError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            InstallError::Schema(errors) => Some(errors),
            InstallError::Store(error) => Some(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::SchemaError;

    fn path() -> Path {
        Path::parse("editor.zoom").expect("test path parses")
    }

    #[test]
    fn a_too_deep_write_names_the_path_and_both_numbers() {
        assert_eq!(
            FieldError::TooDeep {
                path: path(),
                depth: 62,
                max: 61,
            }
            .to_string(),
            "the value at path \"editor.zoom\" would nest 62 containers, past the limit of 61"
        );
    }

    #[test]
    fn a_store_failure_renders_as_the_store_said_it() {
        let inner = RuntimeError::CoreThreadGone;
        assert_eq!(
            FieldError::Store(inner.clone()).to_string(),
            inner.to_string()
        );
        assert_eq!(FieldError::from(inner.clone()), FieldError::Store(inner));
    }

    #[test]
    fn field_error_exposes_its_source_only_where_there_is_one() {
        use std::error::Error as _;
        assert!(
            FieldError::TooDeep {
                path: path(),
                depth: 62,
                max: 61
            }
            .source()
            .is_none()
        );
        assert!(
            FieldError::Store(RuntimeError::CoreThreadGone)
                .source()
                .is_some()
        );
    }

    #[test]
    fn every_install_failure_says_which_stage_failed() {
        let schema = InstallError::Schema(SchemaErrors(vec![SchemaError::Recursive {
            chain: vec!["Node".into(), "Node".into()],
        }]));
        assert_eq!(
            schema.to_string(),
            "invalid schema: recursive type: Node -> Node"
        );
        assert_eq!(
            InstallError::Store(RuntimeError::CoreThreadGone).to_string(),
            format!("could not seed the store: {}", RuntimeError::CoreThreadGone)
        );
    }

    #[test]
    fn every_install_failure_carries_its_source() {
        use std::error::Error as _;
        for error in [
            InstallError::Schema(SchemaErrors(Vec::new())),
            InstallError::Store(RuntimeError::CoreThreadGone),
        ] {
            assert!(error.source().is_some(), "{error:?} should carry a source");
        }
    }
}
