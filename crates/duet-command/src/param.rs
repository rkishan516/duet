//! How a command's arguments are described and decoded: [`CommandParam`].

use duet_protocol::Args;
use duet_schema::{Registry, SharedState, Ty};

/// A type a `#[command]` may take **by value** as an argument.
///
/// # One blanket impl, and why that is the whole design
///
/// The only impl is `impl<T: SharedState> CommandParam for T`. There are no
/// special cases and there is no second impl, so:
///
/// - **Rejection is the absence of a `SharedState` impl.** A parameter typed
///   `u64`, `f32`, `HashSet<T>`, `PathBuf` or `&str` fails to resolve, and the
///   compiler prints `SharedState`'s own `on_unimplemented` note naming the
///   replacement. The macro never inspects a parameter's tokens, so
///   `type Blob = Vec<u8>;` behaves exactly as `Vec<u8>` does — which is the
///   property a syntactic special case would silently break, in the direction
///   of accepting what should have been refused.
/// - **The schema and the decode cannot disagree.** [`param_ty`] returns
///   `T::schema` and [`from_args`] runs `T::from_value`; they are the same
///   type's two obligations rather than two descriptions of one argument.
///
/// [`param_ty`]: CommandParam::param_ty
/// [`from_args`]: CommandParam::from_args
///
/// # Why this trait exists at all, rather than the macro calling `SharedState`
///
/// So the generated code has no logic in it. Every `#[command]` needs the same
/// three-branch decode — absent, present-but-wrong, present-and-right — and the
/// same refusal wording for the first two. Written into each expansion it would
/// be a dozen lines of `match` per parameter that a reviewer has to read and
/// compare; written here it is one call, and the wording is fixed in one place
/// where it can be tested.
pub trait CommandParam: Sized {
    /// How the schema describes this parameter.
    fn param_ty(registry: &mut Registry) -> Ty;

    /// Reads this parameter out of an invocation's arguments.
    ///
    /// # Errors
    ///
    /// The refusal text, ready for
    /// [`Outcome::Refused`](duet_protocol::Outcome::Refused): an argument that
    /// is missing or is not this type means the call never got as far as doing
    /// anything, which is a `failed` and not a `raised`.
    ///
    /// The message names the argument and the *kind* that arrived, never the
    /// value: arguments are guest-chosen and unbounded, and a one-megabyte
    /// string argument must not become a one-megabyte reply. `key` is written
    /// by the host — it is the Rust parameter name, or a `rename` literal — so
    /// it is rendered in full.
    fn from_args(key: &str, args: &Args) -> Result<Self, String>;
}

impl<T: SharedState> CommandParam for T {
    fn param_ty(registry: &mut Registry) -> Ty {
        T::schema(registry)
    }

    fn from_args(key: &str, args: &Args) -> Result<T, String> {
        let Some(value) = args.get(key) else {
            return Err(format!("argument \"{key}\" is missing"));
        };
        // `DecodeError` renders a variant name and a bounded location, never a
        // value, so quoting it whole is safe on this path.
        T::from_value(value)
            .map_err(|why| format!("argument \"{key}\" is not the right type: {why}"))
    }
}

#[cfg(test)]
#[path = "param_tests.rs"]
mod tests;
