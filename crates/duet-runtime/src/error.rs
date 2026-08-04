//! Errors surfaced by the runtime.

use duet_core::SetError;

/// Why a runtime operation could not be completed.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RuntimeError {
    /// The core thread is no longer running, so the request could not be
    /// served and no reply will arrive.
    ///
    /// This means the runtime was shut down, or the core thread panicked. It is
    /// terminal: every subsequent call on the same handle will also fail.
    CoreThreadGone,
    /// The store rejected the write. Carries `duet-core`'s error unchanged.
    Store(SetError),
}

impl std::fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RuntimeError::CoreThreadGone => {
                write!(f, "the runtime core thread is no longer running")
            }
            RuntimeError::Store(e) => write!(f, "store rejected the write: {e}"),
        }
    }
}

impl std::error::Error for RuntimeError {}

impl From<SetError> for RuntimeError {
    fn from(e: SetError) -> Self {
        RuntimeError::Store(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_thread_gone_displays_actionably() {
        let rendered = RuntimeError::CoreThreadGone.to_string();
        assert!(
            rendered.contains("core thread"),
            "message should name the core thread, got: {rendered}"
        );
    }

    #[test]
    fn store_errors_are_wrapped_and_readable() {
        let inner = duet_core::SetError::IndexOutOfBounds {
            path: duet_core::Path::parse("docs[9]").expect("test path should parse"),
            index: 9,
            len: 3,
        };
        let rendered = RuntimeError::Store(inner).to_string();
        assert!(
            rendered.contains('9'),
            "should surface the index, got: {rendered}"
        );
    }

    #[test]
    fn runtime_error_is_a_std_error() {
        fn assert_error<E: std::error::Error>() {}
        assert_error::<RuntimeError>();
    }

    #[test]
    fn set_error_converts_into_a_wrapped_store_error() {
        let inner = duet_core::SetError::MissingKey(
            duet_core::Path::parse("nope.deeper").expect("test path should parse"),
        );
        let converted: RuntimeError = inner.clone().into();
        assert_eq!(
            converted,
            RuntimeError::Store(inner),
            "From must wrap the store error, not discard it"
        );
    }
}
