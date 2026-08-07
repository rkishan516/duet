//! The [`SharedState`] contract, the schema model, and the typed store API.
//!
//! One Rust type definition should yield typed clients in Rust, Dart and
//! TypeScript. This crate is the Rust half of that: the trait a type implements
//! to live in a Duet store, the description of it that `duet-codegen` turns
//! into the other two languages, and the typed handles a Rust application uses
//! to read and write it.
//!
//! # What can be shared
//!
//! | Rust | Schema | `Value` |
//! |---|---|---|
//! | `bool` | `bool` | `Bool` |
//! | `i8` `i16` `i32` `i64` `u8` `u16` `u32` | `int` | `Int` |
//! | `f64` | `float` | `Float` |
//! | `String` | `string` | `Str` |
//! | [`Bytes`] | `bytes` | `Bytes` |
//! | [`duet_core::Value`] | `dynamic` | anything |
//! | `Option<T>` | `optional` | the inner type, or `Null` |
//! | `Vec<T>` | `list` | `List` |
//! | `HashMap<String, V>`, `BTreeMap<String, V>` | `map` | `Map` |
//! | `Box<T>`, `Arc<T>` | the inner type | the inner type |
//! | a struct with a [`SharedState`] impl | `named` | `Map` |
//!
//! Integers narrower than `i64` are **range-checked on decode**. The wire has
//! one integer type; a guest may write `300` to a `u8` path, and that is a
//! reportable mismatch rather than a wrapped number.
//!
//! `Vec<u8>` is accepted, and it lowers to a *list of integers*, not to
//! `Value::Bytes`. Use [`Bytes`] for binary data — the reason the two are
//! separate is coherence, and it is spelled out on [`Bytes`] itself.
//!
//! # What cannot, and why
//!
//! | Refused | Because |
//! |---|---|
//! | `u64` `u128` `i128` `usize` `isize` | `Value::Int` is an `i64`; `u64 > i64::MAX` has no representation, and `usize`/`isize` are platform-width |
//! | `f32` | lossless out, **lossy in**; Dart and TypeScript have no 32-bit float, so it buys nothing across the boundary |
//! | `HashSet<T>` | iteration order is not a function of the value, so `to_value` would not be either, and the output would not be byte-stable |
//! | `HashMap<K, V>`, `K` != `String` | `Value::Map` is keyed by `String`; lowering to a list of pairs would destroy path addressing |
//! | `&str`, `&[T]`, any borrow | the store owns a `'static` tree |
//! | `Rc` `RefCell` `Cell` `Mutex` `RwLock` | two handles to one node become two independent copies once they are values in the tree |
//! | `PathBuf` `OsString` | `OsString` is WTF-8 on Windows; `Value::Str` is UTF-8 only |
//! | `Duration` `SystemTime` `Instant` | no canonical wire spelling — choosing one silently is worse than making the developer choose |
//! | `Option<Option<T>>` | `Some(None)` and `None` both lower to `Null`; the collapse is unrepresentable |
//!
//! ## Rejection is the absence of an impl, never token inspection
//!
//! Every row above is refused by there being **no `SharedState` impl**. A
//! derive macro sees syntax and never resolved types, so a syntactic special
//! case for `Vec<u8>` or `u64` is defeated by `type Blob = Vec<u8>;` — silently,
//! and in the direction of accepting what should have been refused. Trait
//! resolution happens after type resolution and cannot be fooled that way.
//!
//! Each refusal carries a `#[diagnostic::on_unimplemented]` message naming the
//! fix. `tests/rejections.rs` verifies mechanically, at compile time, that every
//! row above genuinely has no impl and none is reachable through a blanket one.
//!
//! # The escape hatch
//!
//! [`SharedState`] is public and hand-implementable. That is why there is no
//! `#[duet(with = ...)]` attribute: a hand-written impl says precisely what an
//! attribute could only gesture at, and it says it to the compiler rather than
//! to a macro that cannot see types. See [`SharedState`] for a worked example.
//!
//! # A complete cycle
//!
//! ```
//! use duet_core::Value;
//! use duet_runtime::{NullSink, Runtime};
//! use duet_schema::{DecodeError, FieldDef, NotNullable, Reading, Registry, Schema,
//!                   SharedState, Ty, install};
//!
//! /// The application's shared state. `duet`'s `derive` feature writes this
//! /// impl for you; it is spelled out here because the trait is meant to be
//! /// hand-implementable, and because this crate sits below the derive.
//! #[derive(Debug, PartialEq)]
//! struct AppState {
//!     counter: i64,
//!     title: Option<String>,
//! }
//!
//! impl SharedState for AppState {
//!     fn to_value(&self) -> Value {
//!         Value::map([
//!             ("counter", self.counter.to_value()),
//!             ("title", self.title.to_value()),
//!         ])
//!     }
//!
//!     fn from_value(value: &Value) -> Result<Self, DecodeError> {
//!         let Value::Map(entries) = value else {
//!             return Err(DecodeError::wrong_type("AppState", value));
//!         };
//!         let counter = entries
//!             .get("counter")
//!             .ok_or_else(|| DecodeError::missing_field("AppState", "counter"))?;
//!         let title = entries
//!             .get("title")
//!             .ok_or_else(|| DecodeError::missing_field("AppState", "title"))?;
//!         Ok(AppState {
//!             counter: i64::from_value(counter).map_err(|e| e.within_key("counter"))?,
//!             title: Option::from_value(title).map_err(|e| e.within_key("title"))?,
//!         })
//!     }
//!
//!     fn schema(registry: &mut Registry) -> Ty {
//!         registry.define::<Self>("AppState", |r| {
//!             vec![
//!                 FieldDef::new("counter", i64::schema(r)),
//!                 FieldDef::new("title", Option::<String>::schema(r)),
//!             ]
//!         })
//!     }
//! }
//!
//! impl NotNullable for AppState {}
//!
//! let runtime = Runtime::spawn(Value::Null, NullSink);
//! let store = install(
//!     runtime.handle(),
//!     &AppState { counter: 0, title: None },
//! )?;
//!
//! let counter = store.field::<i64>("counter")?;
//! counter.set(&41)?;
//! assert_eq!(counter.get()?, Reading::Present(41));
//!
//! // `None` and "no such path" stay distinct, in both directions.
//! let title = store.optional_field::<String>("title")?;
//! assert_eq!(title.get()?, Reading::None);
//! title.set(Some(&"draft".to_string()))?;
//! assert_eq!(title.get()?, Reading::Present("draft".to_string()));
//!
//! // The schema is what `duet-codegen` reads.
//! assert!(Schema::of::<AppState>()?.render().contains("\"name\": \"AppState\""));
//! runtime.shutdown()?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

#![deny(missing_docs)]
#![forbid(unsafe_code)]

mod bytes;
pub mod command;
pub mod decode;
mod echo;
pub mod error;
mod impls;
pub mod registry;
mod render;
pub mod schema;
mod seed;
pub mod state;
pub mod ty;
pub mod typed;

pub use bytes::Bytes;
pub use command::CommandDef;
pub use decode::{DecodeError, DecodeReason};
pub use error::{SchemaError, SchemaErrors};
pub use registry::Registry;
pub use render::SCHEMA_VERSION;
pub use schema::Schema;
pub use seed::seed;
pub use state::{NotNullable, SharedState, SkippedDefault};
pub use ty::{FieldDef, Ty, TypeDef};
pub use typed::error::{FieldError, InstallError};
pub use typed::field::{Field, OptionalField};
pub use typed::reading::{Reading, optional_reading, required_reading};
pub use typed::store::{TypedStore, install};
