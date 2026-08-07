//! Reads a Duet schema and emits typed Dart and TypeScript clients.
//!
//! One Rust type definition should yield typed clients in three languages. This
//! crate is the second half of that: `duet-schema` turns Rust types into a
//! schema document, and this turns a schema document into guest code.
//!
//! # The schema file is the contract, not this crate
//!
//! The document in the middle is the artifact two independent producers have to
//! satisfy — a human who wrote `schema/app.json` by hand, and
//! `#[derive(SharedState)]`, which reproduces it byte for byte. The emitters
//! came first on purpose: had the derive, the format would have been whatever
//! the macro happened to emit, the emitters would have been tested against
//! that, and nothing would independently check either side.
//!
//! The independence is built rather than promised:
//!
//! - `duet-schema`'s [`render`](duet_schema::Schema::render) is a hand-rolled
//!   writer and that crate has **no `serde_json` dependency**. The reader here
//!   is a `serde_json` one. `render → read → compare` is a cross-check between
//!   two implementations that share no code.
//! - The reader hands its result to [`Schema::build`](duet_schema::Schema), so
//!   a parsed schema passes exactly the checks a derived one does.
//!
//! # Three properties the generated code has
//!
//! **Every path is a compile-time string literal**, minted once and validated
//! here against the real path parser. Generated code never assembles a path
//! from a runtime value, which makes
//! [`Path::from_segments`](duet_core::Path::from_segments)' trusted-construction
//! hazard structurally unreachable from it, and makes a golden file greppable
//! for the exact wire string.
//!
//! **A wire key is never rewritten; an accessor name always is.** `snake_case`
//! becomes `lowerCamelCase` in both target languages, and the path keeps the
//! schema's own spelling. Camel-casing a path would give a Dart guest
//! `editor.fontSize` and a Rust host `editor.font_size` — two names for one
//! field, no error anywhere, each guest reading only its own writes. See
//! [`name`].
//!
//! **There are no algorithms in the output.** Decoding, merging, push routing
//! and the absent/null/mismatch distinction all live in the hand-written
//! runtime the generated code delegates to, so a reviewer can check a whole
//! generated diff by reading it.
//!
//! # Totality
//!
//! A schema file is an input like any other and may be malformed or hostile.
//! Nothing here panics, allocates without bound, or recurses without bound; see
//! [`read`] for how, and `tests/fuzz.rs` for the evidence.
//!
//! ```
//! use duet_codegen::{Options, generate, read_schema};
//!
//! let document = r#"{
//!   "root": {"kind": "named", "name": "App"},
//!   "types": [
//!     {"fields": [{"key": "counter", "type": {"kind": "int"}}], "name": "App"}
//!   ],
//!   "version": 1
//! }"#;
//!
//! let schema = read_schema(document)?;
//! let generated = generate(&schema, &Options::new("schema/app.json", "duet generate"))?;
//!
//! // The accessor is camel-cased; the path is the schema's own key.
//! assert!(generated.dart.contains("DuetField<int> get counter"));
//! assert!(generated.dart.contains("router, 'counter'"));
//! assert!(generated.ts.contains("get counter(): DuetField<bigint>"));
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

#![deny(missing_docs)]
#![forbid(unsafe_code)]

mod dart;
pub mod emit;
pub mod error;
pub mod name;
pub mod plan;
pub mod read;
mod ts;

pub use emit::{Generated, Options, TsImports, generate};
pub use error::{EmitError, ReadError};
pub use plan::{MAX_CLASSES, Plan};
pub use read::{KINDS, MAX_TY_DEPTH, SUPPORTED_VERSIONS, read_schema};
