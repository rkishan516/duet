//! Typed `get`, `set` and `subscribe` at one fixed path.

use std::marker::PhantomData;

use duet_core::{MAX_VALUE_DEPTH, Path, PathParseError, SubscriberId, SubscriptionId, Value};
use duet_runtime::{RuntimeError, StoreHandle};

use crate::state::{NotNullable, SharedState};
use crate::typed::error::FieldError;
use crate::typed::reading::{Reading, optional_reading, required_reading};

/// A typed handle on one path that always holds a `T`.
///
/// # The path is a compile-time literal, parsed once
///
/// `Field::new` takes a `&'static str`, so the address cannot be assembled from
/// runtime data. That is what makes
/// [`Path::from_segments`](duet_core::Path::from_segments)' trusted-construction
/// hazard structurally unreachable from generated code: every path a generated
/// client mints has been through [`Path::parse`], and a malformed one is a
/// failure at wiring time rather than on the first read.
///
/// # `T` is what the schema promises, not what the store guarantees
///
/// Another guest can write anything anywhere. A path holding a `Value::Null`,
/// or a string where the schema says an integer, reports
/// [`Reading::Mismatch`]; it is not an error. See [`Reading`].
#[derive(Debug, Clone)]
pub struct Field<T> {
    handle: StoreHandle,
    path: Path,
    literal: &'static str,
    /// `fn() -> T` rather than `T`: a `Field` is `Send`/`Sync` whatever `T` is,
    /// because it holds no `T` — only the promise of one.
    marker: PhantomData<fn() -> T>,
}

/// A typed handle on one path that holds `Option<T>`.
///
/// # Three outcomes, not two
///
/// `None` and "no such path" are different, and this type keeps them different
/// end to end — [`Reading::None`] against [`Reading::Absent`].
///
/// Measured against a real host with an `Option<Editor>` set to `None`, a child
/// path such as `editor.zoom` behaves three different ways at once: `get`
/// answers nothing, `subscribe` succeeds, and `set` **fails** with `path
/// "editor.zoom" addresses the wrong kind of node`. This API does not paper
/// over that, because a `set` that silently no-ops is a lost write the
/// application never learns about.
#[derive(Debug, Clone)]
pub struct OptionalField<T> {
    handle: StoreHandle,
    path: Path,
    literal: &'static str,
    marker: PhantomData<fn() -> T>,
}

/// Generates the shared half of both field types.
///
/// The two differ only in how a value is read and written; everything else —
/// construction, accessors, the depth guard — is identical, and duplicating it
/// would be two places for the depth bound to drift apart.
macro_rules! field_common {
    ($name:ident) => {
        impl<T> $name<T> {
            /// Binds `path` on `handle`.
            ///
            /// # Errors
            ///
            /// [`PathParseError`] if `path` is not a legal path. Generated code
            /// only ever passes a literal the code generator already validated,
            /// so this is reachable in practice only from hand-written wiring.
            pub fn new(handle: StoreHandle, path: &'static str) -> Result<Self, PathParseError> {
                Ok($name {
                    handle,
                    path: Path::parse(path)?,
                    literal: path,
                    marker: PhantomData,
                })
            }

            /// The parsed path this field addresses.
            pub fn path(&self) -> &Path {
                &self.path
            }

            /// The exact literal this field was built from.
            ///
            /// Equal to `self.path().to_string()` — the schema rejects any key
            /// that would not round-trip — and kept so a golden file, a log
            /// line and a wire message can all be grepped for the same string
            /// without a `Path` having to be rendered first.
            pub fn literal(&self) -> &'static str {
                self.literal
            }

            /// The store this field reads and writes.
            pub fn handle(&self) -> &StoreHandle {
                &self.handle
            }

            /// Writes `value` at this field's path, after checking the store
            /// will accept its depth.
            ///
            /// # Why the depth is checked here rather than left to the store
            ///
            /// [`Store::set`](duet_core::Store::set) already refuses a write
            /// that would nest past
            /// [`MAX_VALUE_DEPTH`](duet_core::MAX_VALUE_DEPTH), and it must:
            /// it is the only place that owns the root. But it reports the
            /// refusal as a [`RuntimeError::Store`] arriving from another
            /// thread, which is a surprising shape for a caller who wrote a
            /// typed `set` of a value it already holds. Checking here turns
            /// that into [`FieldError::TooDeep`], names the same two numbers,
            /// and costs no round trip to discover.
            fn write(&self, value: Value) -> Result<(), FieldError> {
                let depth = self.path.segments().len() + value.depth();
                if depth > MAX_VALUE_DEPTH {
                    return Err(FieldError::TooDeep {
                        path: self.path.clone(),
                        depth,
                        max: MAX_VALUE_DEPTH,
                    });
                }
                self.handle
                    .set(&self.path, value)
                    .map_err(FieldError::Store)
            }
        }
    };
}

field_common!(Field);
field_common!(OptionalField);

impl<T: SharedState> Field<T> {
    /// Reads the current value.
    ///
    /// # Errors
    ///
    /// [`RuntimeError`] only when the store could not be reached. A value of
    /// the wrong type is **not** an error; it is a [`Reading::Mismatch`].
    pub fn get(&self) -> Result<Reading<T>, RuntimeError> {
        Ok(required_reading(self.handle.get(&self.path)?))
    }

    /// Writes `value`.
    ///
    /// # Errors
    ///
    /// [`FieldError::TooDeep`] if the value would nest past the store's limit,
    /// and [`FieldError::Store`] if the store refused or could not be reached
    /// — most often because an ancestor of this path does not exist, since the
    /// store never creates intermediate nodes.
    pub fn set(&self, value: &T) -> Result<(), FieldError> {
        self.write(value.to_value())
    }

    /// Subscribes to this path, returning the subscription and its snapshot.
    ///
    /// The snapshot is a [`Reading`] like any other, so a subscription that
    /// starts on a path holding the wrong type reports that immediately rather
    /// than on the first change.
    ///
    /// # Errors
    ///
    /// [`RuntimeError`] if the store could not be reached.
    pub fn subscribe(
        &self,
        subscriber: SubscriberId,
    ) -> Result<(SubscriptionId, Reading<T>), RuntimeError> {
        let (id, snapshot) = self.handle.subscribe(subscriber, self.path.clone())?;
        Ok((id, required_reading(snapshot)))
    }
}

impl<T: SharedState + NotNullable> OptionalField<T> {
    /// Reads the current value.
    ///
    /// # Errors
    ///
    /// [`RuntimeError`] only when the store could not be reached.
    pub fn get(&self) -> Result<Reading<T>, RuntimeError> {
        Ok(optional_reading(self.handle.get(&self.path)?))
    }

    /// Writes `value`, or [`Value::Null`] — Rust's `None` — when it is `None`.
    ///
    /// There is no way to make the path *absent* again, and that is not an
    /// omission: the wire has no delete. `None` and "no such path" are
    /// different states, and only the shape of the enclosing value decides the
    /// second.
    ///
    /// # Errors
    ///
    /// As [`Field::set`].
    pub fn set(&self, value: Option<&T>) -> Result<(), FieldError> {
        self.write(match value {
            Some(inner) => inner.to_value(),
            None => Value::Null,
        })
    }

    /// Subscribes to this path, returning the subscription and its snapshot.
    ///
    /// # Errors
    ///
    /// [`RuntimeError`] if the store could not be reached.
    pub fn subscribe(
        &self,
        subscriber: SubscriberId,
    ) -> Result<(SubscriptionId, Reading<T>), RuntimeError> {
        let (id, snapshot) = self.handle.subscribe(subscriber, self.path.clone())?;
        Ok((id, optional_reading(snapshot)))
    }
}
