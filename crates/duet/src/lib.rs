//! Duet: one Rust type definition, typed clients in Rust, Dart and TypeScript.
//!
//! A Rust host owns the shared state; a Flutter engine and a `wry` webview are
//! guest renderers reading and writing it over a tagged JSON wire. This crate is
//! the front door — depend on `duet` and nothing else.
//!
//! # What is where
//!
//! | Module | Crate | What it holds |
//! |---|---|---|
//! | [`core`] | `duet-core` | [`Value`], [`Path`], `Store`, lifecycle, policy |
//! | [`runtime`] | `duet-runtime` | [`Runtime`], [`StoreHandle`], `Sink` |
//! | [`schema`] | `duet-schema` | [`SharedState`], [`Schema`], [`Field`], [`install`] |
//!
//! The split exists for one reason: **`duet-core` has no dependencies at all**,
//! and it is meant to stay that way, so the parts of Duet that need one live
//! elsewhere. `cargo tree -p duet-core` printing a single line is asserted in
//! CI, not merely intended.
//!
//! # Start here
//!
//! [`SharedState`] is the contract a type implements to live in the store —
//! including the table of what Duet accepts and refuses, and why. [`install`]
//! seeds a store and hands back a [`TypedStore`]. [`Field`] and
//! [`OptionalField`] are the typed handles.
//!
//! ```
//! use duet::{Reading, Runtime, Value, install};
//! use duet::runtime::NullSink;
//!
//! let runtime = Runtime::spawn(Value::Null, NullSink);
//! let store = install(runtime.handle(), &7i64)?;
//! assert_eq!(store.field::<i64>("")?.get()?, Reading::Present(7));
//! runtime.shutdown()?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub use duet_core as core;
pub use duet_runtime as runtime;
pub use duet_schema as schema;

pub use duet_core::{
    MAX_VALUE_DEPTH, Path, PathParseError, Segment, SetError, Store, SubscriberId, SubscriptionId,
    Value,
};
pub use duet_runtime::{Runtime, RuntimeError, Sink, StoreHandle};
pub use duet_schema::{
    Bytes, DecodeError, Field, FieldDef, FieldError, InstallError, NotNullable, OptionalField,
    Reading, Registry, Schema, SchemaError, SchemaErrors, SharedState, Ty, TypeDef, TypedStore,
    install,
};
