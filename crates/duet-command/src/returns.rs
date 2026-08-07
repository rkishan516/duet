//! What a command hands back, described and lowered: [`CommandReturn`].

use duet_core::Value;
use duet_protocol::Outcome;
use duet_schema::{Registry, SharedState, Ty};

/// Which of [`CommandReturn`]'s four impls applies to a return type.
///
/// # These exist only so coherence permits four impls
///
/// `CommandReturn` needs to cover four unrelated shapes — `T`, `()`,
/// `Result<T, E>` and `Result<(), E>` — and two of them overlap the blanket one
/// syntactically. Rust's coherence rules have no negative reasoning: the
/// compiler cannot use "`()` does not implement `SharedState`" to rule out
/// `impl<T: SharedState> CommandReturn for T` as a candidate for `()`, so
/// writing those two impls without a distinguishing parameter is a
/// "conflicting implementations" error before any user writes any command.
///
/// A type parameter the trait carries and nobody ever names fixes that:
/// `CommandReturn<Plain>` and `CommandReturn<Nothing>` are different traits as
/// far as coherence is concerned. The macro writes `_` for it and the compiler
/// infers which one applies.
///
/// # What the markers do not buy
///
/// Inference is unambiguous only while **at most one impl applies** to each
/// return type, and that is a property of `duet-schema`, not of this module: it
/// holds because neither `()` nor `Result<T, E>` implements `SharedState`.
/// `crates/duet-schema/tests/rejections.rs` asserts both, mechanically, for
/// exactly this reason — a `SharedState` impl added for either would turn every
/// `#[command]` returning that shape into "type annotations needed", pointing
/// into generated code for a change made in another crate.
pub mod marker {
    /// A command returning a plain value: `-> i64`.
    #[derive(Debug, Clone, Copy)]
    pub struct Plain;
    /// A command returning nothing: `-> ()`, or no `->` at all.
    #[derive(Debug, Clone, Copy)]
    pub struct Nothing;
    /// A command returning `Result<T, E>`.
    #[derive(Debug, Clone, Copy)]
    pub struct Fallible;
    /// A command returning `Result<(), E>`.
    #[derive(Debug, Clone, Copy)]
    pub struct FallibleNothing;
}

/// A type a `#[command]` may return.
///
/// One trait doing two jobs that must agree: it says what the **schema**
/// records for a command's reply, and it turns the value a body produced into
/// the [`Outcome`] the wire carries. A `returns` of `int` and an `into_outcome`
/// that produced a string would be a generated client decoding the wrong node,
/// and keeping both on one impl is what stops that being expressible.
///
/// | Rust return | `returns` | `raises` | `Outcome` |
/// |---|---|---|---|
/// | `T: SharedState` | `T` | absent | `Returned(T)` |
/// | `()` | absent | absent | `Returned(Null)` |
/// | `Result<T, E>` | `T` | `E` | `Returned(T)` / `Raised(E)` |
/// | `Result<(), E>` | absent | `E` | `Returned(Null)` / `Raised(E)` |
///
/// # `Refused` is not in that table, on purpose
///
/// A command body cannot produce an [`Outcome::Refused`]. Refusal means the
/// host would not run the call at all — an unknown name, an argument that did
/// not decode — and by the time a body is running, all of that has already
/// succeeded. A body that wants to report failure returns `Err`, which the
/// guest receives as `raised`: a command that *ran and failed*, which is a
/// different event and the one the wire's two reply kinds exist to separate.
///
/// The consequence is worth stating plainly, because it is a real constraint on
/// what `#[command]` can express: a hand-written [`CommandHost`] can refuse from
/// inside a body and a `#[command]` cannot. A hand-written body that does so is
/// usually decoding an argument by hand, and the `#[command]` spelling of the
/// same thing is a parameter type whose `SharedState::from_value` rejects it —
/// which is a refusal, produced by [`CommandParam`](crate::CommandParam), at
/// the point the argument arrives.
///
/// [`CommandHost`]: duet_protocol::CommandHost
pub trait CommandReturn<Marker> {
    /// What a `returned` reply carries, or `None` when there is no return type.
    fn returns(registry: &mut Registry) -> Option<Ty>;

    /// What a `raised` reply carries, or `None` when the command cannot fail.
    fn raises(registry: &mut Registry) -> Option<Ty>;

    /// Lowers the value a body produced onto the wire.
    fn into_outcome(self) -> Outcome;
}

impl<T: SharedState> CommandReturn<marker::Plain> for T {
    fn returns(registry: &mut Registry) -> Option<Ty> {
        Some(T::schema(registry))
    }

    fn raises(_registry: &mut Registry) -> Option<Ty> {
        None
    }

    fn into_outcome(self) -> Outcome {
        Outcome::Returned(self.to_value())
    }
}

impl CommandReturn<marker::Nothing> for () {
    fn returns(_registry: &mut Registry) -> Option<Ty> {
        None
    }

    fn raises(_registry: &mut Registry) -> Option<Ty> {
        None
    }

    /// [`Value::Null`], which is what `Outcome::Returned` documents for a
    /// command with no result. The schema records no return *type*; the wire
    /// still carries a value, because every `invoke` is answered.
    fn into_outcome(self) -> Outcome {
        Outcome::Returned(Value::Null)
    }
}

impl<T: SharedState, E: SharedState> CommandReturn<marker::Fallible> for Result<T, E> {
    fn returns(registry: &mut Registry) -> Option<Ty> {
        Some(T::schema(registry))
    }

    fn raises(registry: &mut Registry) -> Option<Ty> {
        Some(E::schema(registry))
    }

    fn into_outcome(self) -> Outcome {
        match self {
            Ok(value) => Outcome::Returned(value.to_value()),
            Err(error) => Outcome::Raised(error.to_value()),
        }
    }
}

impl<E: SharedState> CommandReturn<marker::FallibleNothing> for Result<(), E> {
    fn returns(_registry: &mut Registry) -> Option<Ty> {
        None
    }

    fn raises(registry: &mut Registry) -> Option<Ty> {
        Some(E::schema(registry))
    }

    fn into_outcome(self) -> Outcome {
        match self {
            Ok(()) => Outcome::Returned(Value::Null),
            Err(error) => Outcome::Raised(error.to_value()),
        }
    }
}

/// What the schema records as `R`'s `returns`.
///
/// A free function rather than a call through the trait, so generated code
/// writes `command_returns::<i64, _>(registry)` and lets the marker be
/// inferred, instead of naming a type parameter no user has ever heard of.
pub fn command_returns<R: CommandReturn<M>, M>(registry: &mut Registry) -> Option<Ty> {
    R::returns(registry)
}

/// What the schema records as `R`'s `raises`.
pub fn command_raises<R: CommandReturn<M>, M>(registry: &mut Registry) -> Option<Ty> {
    R::raises(registry)
}

/// Lowers a body's return value onto the wire.
pub fn into_outcome<R: CommandReturn<M>, M>(value: R) -> Outcome {
    value.into_outcome()
}

#[cfg(test)]
#[path = "returns_tests.rs"]
mod tests;
