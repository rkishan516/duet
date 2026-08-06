//! A typed view of a running store, and the call that seeds one.

use duet_core::Path;
use duet_runtime::StoreHandle;

use crate::schema::Schema;
use crate::state::{NotNullable, SharedState};
use crate::typed::error::InstallError;
use crate::typed::field::{Field, OptionalField};

/// A typed view of one store.
///
/// Cheap to clone — it holds a [`StoreHandle`], which is itself a channel
/// sender — and usable from any thread. It is a *factory* rather than a
/// container: it mints [`Field`]s and [`OptionalField`]s, and holds no state of
/// its own, so nothing here can fall out of sync with the store.
#[derive(Debug, Clone)]
pub struct TypedStore {
    handle: StoreHandle,
}

impl TypedStore {
    /// Wraps `handle`.
    ///
    /// Does **not** check that the store's contents match any schema, because
    /// nothing could keep that true: another guest may write anything at any
    /// moment. The type-level promise is kept per read, as a [`Reading`](crate::Reading).
    pub fn new(handle: StoreHandle) -> TypedStore {
        TypedStore { handle }
    }

    /// The underlying store handle, for the untyped operations
    /// ([`unsubscribe`](StoreHandle::unsubscribe),
    /// [`next_subscriber_id`](StoreHandle::next_subscriber_id)) that have no
    /// typed counterpart.
    pub fn handle(&self) -> &StoreHandle {
        &self.handle
    }

    /// Binds a required field at `path`.
    ///
    /// # Errors
    ///
    /// [`PathParseError`](duet_core::PathParseError) if `path` is not legal.
    /// Generated code passes only literals the generator validated, so this is
    /// reachable in practice from hand-written wiring alone.
    pub fn field<T: SharedState>(
        &self,
        path: &'static str,
    ) -> Result<Field<T>, duet_core::PathParseError> {
        Field::new(self.handle.clone(), path)
    }

    /// Binds an optional field at `path`.
    ///
    /// # Errors
    ///
    /// As [`TypedStore::field`].
    pub fn optional_field<T: SharedState + NotNullable>(
        &self,
        path: &'static str,
    ) -> Result<OptionalField<T>, duet_core::PathParseError> {
        OptionalField::new(self.handle.clone(), path)
    }
}

/// Validates `T`'s schema, seeds `handle`'s store with `state`, and returns a
/// typed view of it.
///
/// # Why the seed write is not optional
///
/// [`Store::set`](duet_core::Store::set) never creates intermediate nodes, and
/// the store has no `remove`. So a field is writable only if every ancestor on
/// its path already exists, which means **every schema field must be
/// materialized up front** — a `None` as `Value::Null`, an empty list as an
/// empty list. `T::to_value` does exactly that by construction, since it emits
/// every field of every struct it reaches. Installing the root is what turns
/// "key absent" from a representable state into a schema violation.
///
/// # Why the schema is validated here
///
/// A cycle, a name collision or a key that does not round-trip through
/// [`Path::parse`](duet_core::Path) makes every generated client wrong. Finding
/// it at startup, before a guest connects, costs one walk of a small graph;
/// finding it later costs a debugging session over a wire message that
/// addresses the wrong node.
///
/// # Errors
///
/// - [`InstallError::Schema`] — `T`'s schema is not valid; every problem found
///   is carried, not just the first.
/// - [`InstallError::Store`] — the store refused the seed write or could not be
///   reached.
///
/// # Example
///
/// ```
/// use duet_core::{Path, Value};
/// use duet_runtime::{NullSink, Runtime};
/// use duet_schema::{Reading, install};
///
/// let runtime = Runtime::spawn(Value::Null, NullSink);
/// let store = install(runtime.handle(), &7i64).expect("a bare i64 is a legal root");
///
/// let root = store.field::<i64>("").expect("the empty path is the root");
/// assert_eq!(root.get().unwrap(), Reading::Present(7));
/// runtime.shutdown().unwrap();
/// ```
pub fn install<T: SharedState>(handle: StoreHandle, state: &T) -> Result<TypedStore, InstallError> {
    Schema::of::<T>().map_err(InstallError::Schema)?;
    handle
        .set(&Path::root(), state.to_value())
        .map_err(InstallError::Store)?;
    Ok(TypedStore::new(handle))
}
