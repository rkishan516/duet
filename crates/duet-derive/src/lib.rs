//! `#[derive(SharedState)]`: one Rust type definition, one Duet schema.
//!
//! Do not depend on this crate directly. It is re-exported by `duet` behind the
//! `derive` feature, which is the dependency a user writes:
//!
//! ```toml
//! duet = { version = "0.1", features = ["derive"] }
//! ```
//!
//! ```
//! use duet::{Schema, SharedState};
//!
//! #[derive(SharedState)]
//! struct App {
//!     counter: i64,
//!     editor: Editor,
//!     title: String,
//! }
//!
//! #[derive(SharedState)]
//! struct Editor {
//!     zoom: f64,
//!     theme: String,
//! }
//!
//! // The same bytes `schema/app.json` holds, which `duet-codegen` turns into
//! // the Dart and TypeScript clients.
//! let rendered = Schema::of::<App>()?.render();
//! assert!(rendered.contains(r#"{"key": "counter", "type": {"kind": "int"}}"#));
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # What it generates
//!
//! An `impl SharedState` that calls `<FieldTy as SharedState>::to_value`,
//! `::from_value` and `::schema` once per field, plus an `impl NotNullable`
//! (a struct lowers to a `Value::Map` and never to `Value::Null`, so
//! `Option<TheStruct>` is expressible).
//!
//! # Rejection is the absence of an impl, never token inspection
//!
//! The derive **never looks at a field's type**. `u64`, `f32`, `HashSet<T>`,
//! `PathBuf`, `Rc<T>`, `&str` and the rest are refused because `duet-schema`
//! implements `SharedState` for none of them, so `<u64 as SharedState>::schema`
//! does not resolve and the compiler says so — with the
//! `#[diagnostic::on_unimplemented]` note naming the replacement.
//!
//! That is not a stylistic preference. A derive sees **syntax**, never resolved
//! types, so a syntactic special case for `Vec<u8>` is defeated by
//! `type Blob = Vec<u8>;` — silently, and in the direction of accepting what
//! should have been refused. Trait resolution happens after type resolution and
//! cannot be fooled that way. `tests/ui/` holds one compile-fail case per
//! refusal with its message committed.
//!
//! # The three attributes
//!
//! ```
//! # use duet::SharedState;
//! #[derive(SharedState)]
//! # #[duet(crate = ::duet)]
//! struct Window {
//!     /// Shared under the wire key `window_title` rather than `title`.
//!     #[duet(rename = "window_title")]
//!     title: String,
//!     /// Not shared state at all. Requires `Default`, which is what fills it
//!     /// in when the struct is decoded back out of the store.
//!     #[duet(skip)]
//!     cached_extent: Option<f64>,
//! }
//! ```
//!
//! `#[duet(crate = ::my_reexport)]` on the struct redirects every path in the
//! generated code, for a crate that re-exports `duet` under its own name. The
//! path must resolve to a module exporting `SharedState`, `NotNullable`,
//! `SkippedDefault`, `Registry`, `Ty`, `FieldDef`, `DecodeError` and `Value` —
//! everything `duet`'s own root exports, so `pub use duet::*;` satisfies it.
//!
//! There is deliberately **no `#[duet(with = ...)]`**: see `duet::SharedState`'s
//! own documentation for why a hand-written impl is the escape hatch instead.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

mod attr;
#[cfg(feature = "commands")]
mod command;
mod errors;
mod generate;
mod keys;
mod model;

use proc_macro2::TokenStream;

/// Derives `duet::SharedState` for a struct with named fields.
///
/// See the [crate documentation](crate) for what it generates, the three
/// `#[duet(...)]` attributes, and why a rejected field type produces a trait
/// error rather than a macro error.
#[proc_macro_derive(SharedState, attributes(duet))]
pub fn derive_shared_state(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    expand(input.into()).into()
}

/// The whole macro, on `proc_macro2` tokens so it is callable from a test.
///
/// The `#[proc_macro_derive]` shim above cannot be: `proc_macro::TokenStream`
/// only exists inside a real compilation, so a unit test can reach this and not
/// that. Everything worth testing is therefore on this side of the line.
///
/// A refusal expands to `compile_error!` invocations — one per problem, each
/// spanned on the token that caused it — and **never** to a panic. A panicking
/// proc macro reports "custom attribute panicked" with the macro's own
/// backtrace and no span into the user's file, which is the worst diagnostic in
/// the language to be handed for a misspelled attribute.
fn expand(input: TokenStream) -> TokenStream {
    match model::Model::parse(input) {
        Ok(model) => generate::impls(&model),
        Err(errors) => errors.to_compile_error(),
    }
}

/// Describes a Rust function as a host command a Duet guest may invoke.
///
/// Available with the `commands` feature, which `duet`'s own `commands` feature
/// turns on:
///
/// ```toml
/// duet = { version = "0.1", features = ["derive", "commands"] }
/// ```
///
/// ```text
/// #[command]
/// fn subtract(a: i64, b: i64) -> i64 { a - b }
///
/// #[command]
/// fn bump(ctx: &CommandContext, path: String, by: i64) -> Result<i64, BumpError> { … }
///
/// static COMMANDS: [CommandEntry; 2] = commands![subtract, bump];
/// ```
///
/// # What it generates
///
/// The function, unchanged, plus a hidden `struct` of the same name carrying an
/// `impl duet::Command`. The function stays callable from Rust exactly as it
/// was; the macro adds a description of it beside it rather than replacing it.
///
/// # Arguments by value, the context by reference
///
/// A parameter written **by value** is an argument: it is decoded out of the
/// invocation's `args` under its own name, and its type must implement
/// `SharedState`. A parameter written **by reference** is the invocation's
/// context, and its type must be `&CommandContext`.
///
/// That is the one decision made from syntax, and it has to be — a macro sees
/// tokens and never resolved types. *What* either one is remains a question for
/// trait resolution, so `u64` is refused by having no `SharedState` impl and
/// `&str` by having no `FromContext` impl, and a type alias for either changes
/// nothing.
///
/// # The two attributes
///
/// ```text
/// #[command(rename = "documents.add", crate = ::my_reexport)]
/// fn add(#[duet(rename = "window_title")] title: String) { … }
/// ```
///
/// `#[derive(SharedState)]`'s third attribute, `skip`, has no meaning here: a
/// command's arguments are the whole of its input, so a skipped one would be an
/// argument the guest cannot supply and the body still requires.
#[cfg(feature = "commands")]
#[proc_macro_attribute]
pub fn command(
    attr: proc_macro::TokenStream,
    item: proc_macro::TokenStream,
) -> proc_macro::TokenStream {
    command::expand(attr.into(), item.into()).into()
}

#[cfg(test)]
#[path = "expand_tests.rs"]
mod tests;
