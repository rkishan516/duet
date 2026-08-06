//! The [`SharedState`] contract and the [`NotNullable`] marker.

use duet_core::Value;

use crate::decode::DecodeError;
use crate::registry::Registry;
use crate::ty::Ty;

/// A Rust type that can live in a Duet store and be described to the guests.
///
/// # Three obligations, one type
///
/// - [`to_value`](SharedState::to_value) lowers a value onto the store's
///   dynamic tree.
/// - [`from_value`](SharedState::from_value) reads it back, **totally**: a
///   guest can write any value to any path, so this is a decoder for hostile
///   input, not a deserializer for data you wrote.
/// - [`schema`](SharedState::schema) describes the type so `duet-codegen` can
///   emit the matching Dart and TypeScript.
///
/// The three must agree. `from_value(to_value(x)) == x` is the property the
/// impls in this crate are tested against, and the schema must describe exactly
/// what `to_value` produces — a disagreement there is a Rust client and a Dart
/// client that read the same path differently.
///
/// # Implement it by hand when the derive says no
///
/// This trait is public and hand-implementable **on purpose**. It is the
/// documented escape hatch for a type Duet does not accept: a newtype over
/// `SystemTime`, a domain enum, a type from a crate you do not own. That is why
/// there is no `#[duet(with = ...)]` attribute — an attribute would be a second,
/// weaker way to say what a hand-written impl already says precisely, and it
/// would have to be understood by a macro that cannot see types.
///
/// ```
/// use duet_core::Value;
/// use duet_schema::{DecodeError, NotNullable, Registry, SharedState, Ty};
///
/// /// A duration the application chose to spell as whole milliseconds.
/// #[derive(Debug, PartialEq)]
/// struct Millis(i64);
///
/// impl SharedState for Millis {
///     fn to_value(&self) -> Value {
///         Value::Int(self.0)
///     }
///
///     fn from_value(value: &Value) -> Result<Self, DecodeError> {
///         match value {
///             Value::Int(n) => Ok(Millis(*n)),
///             other => Err(DecodeError::wrong_type("Millis", other)),
///         }
///     }
///
///     fn schema(_registry: &mut Registry) -> Ty {
///         Ty::Int
///     }
/// }
///
/// // Required only so `Option<Millis>` is expressible; see `NotNullable`.
/// impl NotNullable for Millis {}
///
/// assert_eq!(Millis::from_value(&Value::Int(7)), Ok(Millis(7)));
/// assert!(Millis::from_value(&Value::Null).is_err());
/// ```
///
/// # Why rejection is the *absence* of an impl
///
/// Duet refuses `u64`, `f32`, `HashSet<T>` and the rest by simply not
/// implementing this trait for them. It never inspects tokens. A derive macro
/// sees syntax, never resolved types, so a syntactic special case for `Vec<u8>`
/// is defeated by `type Blob = Vec<u8>;` — and worse, defeated *silently*, in
/// the direction of accepting something that should have been refused. Trait
/// resolution happens after type resolution and cannot be fooled that way.
///
/// The full list of accepted and rejected types is in the crate documentation.
#[diagnostic::on_unimplemented(
    message = "`{Self}` cannot be shared through a Duet store",
    label = "no `SharedState` impl exists for `{Self}`",
    note = "Duet refuses a type by not implementing `SharedState` for it. The usual fixes:\n\
            \x20 `u64` `u128` `i128` `usize` `isize`  ->  `i64` (the wire's only integer is `i64`; `u64 > i64::MAX` has no representation)\n\
            \x20 `f32`                                ->  `f64` (Dart and TS have no 32-bit float, so `f32` is lossy inbound for nothing)\n\
            \x20 `HashSet<T>`                         ->  `Vec<T>` or `BTreeSet` via a hand impl (hash order is not a function of the value)\n\
            \x20 `HashMap<K, V>` with `K` != `String` ->  `HashMap<String, V>` (`Value::Map` has string keys; a list of pairs loses path addressing)\n\
            \x20 `Vec<u8>` as binary                  ->  `duet::Bytes` (a bare `Vec<u8>` is accepted, but lowers to a list of ints)\n\
            \x20 `&str` `&[T]` and other borrows      ->  the owned type (the store owns a `'static` tree)\n\
            \x20 `Rc` `RefCell` `Cell` `Mutex` `RwLock` -> the inner type (two handles to one node become two copies through `Value`)\n\
            \x20 `PathBuf` `OsString`                 ->  `String` (`OsString` is WTF-8 on Windows; `Value::Str` is UTF-8 only)\n\
            \x20 `Duration` `SystemTime` `Instant`    ->  an explicit `i64` or `String` encoding you choose\n\
            \x20 `Option<Option<T>>`                  ->  `Option<T>` (`Some(None)` and `None` both lower to `Value::Null`)\n\
            For anything else, write `impl SharedState for {Self}` yourself — it is a supported escape hatch, not a workaround."
)]
pub trait SharedState: Sized {
    /// Lowers `self` onto the store's dynamic tree.
    ///
    /// Must be a **function of the value**: two equal values must produce
    /// byte-identical output. That is why `HashSet` is refused and
    /// `HashMap<String, V>` is not — the set's iteration order would leak into
    /// a `Value::List`, while the map's keys are collected into a
    /// `BTreeMap` that sorts them.
    ///
    /// Must be total: a `to_value` that can panic turns a typed `set` into a
    /// crash on data the application already holds.
    fn to_value(&self) -> Value;

    /// Reads `value` back, or explains why it is not a `Self`.
    ///
    /// # Errors
    ///
    /// [`DecodeError`] whenever `value` is not this type. **Every** `Value` is
    /// a legal argument: another guest can write anything to any path, so this
    /// must answer for `Value::Bytes` where a struct was expected just as
    /// calmly as for a well-formed one. It must never panic.
    fn from_value(value: &Value) -> Result<Self, DecodeError>;

    /// Describes this type, registering any struct definitions it needs.
    ///
    /// A primitive ignores `registry` and returns a [`Ty`] directly; a struct
    /// calls [`Registry::define`] and returns the [`Ty::Named`] it hands back.
    fn schema(registry: &mut Registry) -> Ty;
}

/// A [`SharedState`] type whose [`to_value`](SharedState::to_value) never
/// produces [`Value::Null`] at the top level.
///
/// # The one thing this exists for
///
/// `Option<T>` lowers `None` to `Value::Null`. If `T` could itself lower to
/// `Value::Null`, then two distinct Rust values would produce the same
/// `Value` and no decoder could tell them apart:
///
/// ```text
/// Option::<Option<i64>>::None        -> Value::Null
/// Option::Some(Option::<i64>::None)  -> Value::Null
/// ```
///
/// The collapse is not a limitation of the encoding that a cleverer encoding
/// would fix — the store has no third spelling to spend on it, because
/// `Value::Null` *is* how the tree says "nothing here" and there is no
/// `remove`. So `Option<T>` requires `T: NotNullable`, and `Option<Option<T>>`
/// simply has no impl. Flatten it.
///
/// The same argument rules out `Option<duet_core::Value>`: a `Value` may *be*
/// `Value::Null`, so `Some(Value::Null)` and `None` would collide identically.
/// [`Value`] is therefore [`SharedState`] but deliberately not `NotNullable`.
///
/// # Implementing it
///
/// A hand-written [`SharedState`] impl should add `impl NotNullable for MyType
/// {}` whenever its `to_value` cannot return `Value::Null` — which is almost
/// always. Forgetting it is safe: the cost is that `Option<MyType>` does not
/// compile, which is a visible, immediate false rejection rather than a silent
/// wrong answer.
#[diagnostic::on_unimplemented(
    message = "`{Self}` may lower to `Value::Null`, so it cannot go inside an `Option`",
    label = "`Option<{Self}>` would collapse `Some(null)` and `None` into one value",
    note = "If `{Self}` is `Option<T>`, flatten it: nested options have no representation in the store.\n\
            If `{Self}` is `duet_core::Value`, drop the `Option`: a `Value` can already be `Value::Null`.\n\
            If `{Self}` is your own type, add an empty `impl NotNullable for {Self}` next to its `SharedState` impl."
)]
pub trait NotNullable {}
