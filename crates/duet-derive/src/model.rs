//! What the derive understood: a struct with named fields, and the wire key
//! each of them occupies.
//!
//! Everything the macro is allowed to *decide* happens here. The generator
//! below it does no thinking — it walks this model and writes tokens — which is
//! what keeps the expanded output a list of field descriptions a reviewer can
//! check by reading the diff.

use proc_macro2::{Span, TokenStream};
use syn::punctuated::Punctuated;

use crate::attr;
use crate::errors::Errors;
use crate::keys;

/// A struct the derive accepted, ready to be written out.
pub struct Model {
    /// The Rust type's identifier, for `impl ... for #ident`.
    pub ident: syn::Ident,
    /// Its schema type name: the identifier with any `r#` stripped.
    pub name: String,
    /// The path the generated code names Duet by.
    pub krate: syn::Path,
    /// Every field, in **declaration order** — which is the order the schema
    /// records, and therefore the order generated Dart and TypeScript
    /// constructors take their positional arguments in.
    pub fields: Vec<Field>,
}

/// One field of the struct.
pub struct Field {
    /// The Rust field name, for `self.#ident`.
    pub ident: syn::Ident,
    /// Its declared type, for `<#ty as SharedState>::...`.
    pub ty: syn::Type,
    /// Whether it is shared state, and under what key.
    pub role: Role,
}

/// Whether a field reaches the store.
pub enum Role {
    /// Shared, under this wire key.
    Shared {
        /// The map key this field occupies, and one segment of every path that
        /// addresses it.
        key: String,
        /// Where to point a complaint about that key: the `rename` literal when
        /// there was one, the field name when there was not.
        span: Span,
    },
    /// `#[duet(skip)]`: absent from the wire and from the schema, filled in by
    /// `Default` on the way back.
    Skipped,
}

impl Field {
    /// This field's wire key, or `None` when it is skipped.
    pub fn key(&self) -> Option<&str> {
        match &self.role {
            Role::Shared { key, .. } => Some(key),
            Role::Skipped => None,
        }
    }
}

impl Model {
    /// Reads a `#[derive(SharedState)]` input, or explains why it cannot.
    ///
    /// # Errors
    ///
    /// Every problem found, combined — a struct with three bad attributes
    /// reports three, not the first.
    pub fn parse(input: TokenStream) -> Result<Model, syn::Error> {
        let input: syn::DeriveInput = syn::parse2(input)?;
        let mut errors = Errors::new();
        let container = attr::Container::parse(&input.attrs, &mut errors);
        reject_generics(&input.generics, &mut errors);

        let fields = match named_fields(&input, &mut errors) {
            Some(named) => read_fields(named, &mut errors),
            None => Vec::new(),
        };
        keys::check(&fields, &mut errors);

        errors.finish(Model {
            name: unraw(&input.ident),
            ident: input.ident,
            krate: container.krate.unwrap_or_else(duet_path),
            fields,
        })
    }
}

/// `Foo` for `Foo`, and `struct` for `r#struct`.
///
/// A raw identifier's `r#` is Rust syntax for "this word is a keyword", not
/// part of the name. Leaving it on would put `r#type` on the wire and in every
/// generated Dart accessor, which is a Rust spelling leaking into two languages
/// that have never heard of it.
pub fn unraw(ident: &syn::Ident) -> String {
    let rendered = ident.to_string();
    match rendered.strip_prefix("r#") {
        Some(bare) => bare.to_string(),
        None => rendered,
    }
}

/// `::duet`, the path the generated code uses unless `#[duet(crate = ...)]`
/// says otherwise.
///
/// Built by hand rather than through `parse_quote!`, which panics on a parse
/// failure — a proc macro must never panic, and "it cannot fail here" is an
/// argument, not a guarantee.
pub fn duet_path() -> syn::Path {
    let mut segments = Punctuated::new();
    segments.push(syn::PathSegment {
        ident: syn::Ident::new("duet", Span::call_site()),
        arguments: syn::PathArguments::None,
    });
    syn::Path {
        leading_colon: Some(syn::token::PathSep::default()),
        segments,
    }
}

/// Refuses a generic type.
///
/// `Registry::define` keys a definition by **name**, and a generic type has one
/// identifier for every instantiation: `Holder<i64>` and `Holder<String>` would
/// both claim `Holder`, and the registry would report a name collision at
/// startup for a mistake made in the type definition. There is no spelling in
/// the schema for "the same struct at two element types".
fn reject_generics(generics: &syn::Generics, errors: &mut Errors) {
    if generics.params.is_empty() && generics.where_clause.is_none() {
        return;
    }
    errors.at(
        generics,
        "`#[derive(SharedState)]` cannot describe a generic type: the schema names a struct once, \
         and every instantiation of a generic would claim that one name.\n\
         Fixes: derive it on a concrete struct that wraps the instantiation you share \
         (`struct Doc { items: Vec<i64> }`), or write `impl SharedState` for each instantiation \
         by hand.",
    );
}

/// The struct's named fields, or the reason there are none.
fn named_fields<'a>(
    input: &'a syn::DeriveInput,
    errors: &mut Errors,
) -> Option<&'a Punctuated<syn::Field, syn::token::Comma>> {
    match &input.data {
        syn::Data::Struct(data) => named_of(data, &input.ident, errors),
        syn::Data::Enum(data) => {
            errors.at(data.enum_token, shape_error("an enum", ONE_OF_SEVERAL));
            None
        }
        syn::Data::Union(data) => {
            errors.at(data.union_token, shape_error("a union", ONE_OF_SEVERAL));
            None
        }
    }
}

/// Why a choice between shapes has no schema.
const ONE_OF_SEVERAL: &str = "The schema describes one shape per name, and has no arm for a value \
                              that is one of several shapes.";

/// A struct's fields, if they are named.
fn named_of<'a>(
    data: &'a syn::DataStruct,
    ident: &syn::Ident,
    errors: &mut Errors,
) -> Option<&'a Punctuated<syn::Field, syn::token::Comma>> {
    match &data.fields {
        syn::Fields::Named(named) => Some(&named.named),
        syn::Fields::Unnamed(unnamed) => {
            errors.at(
                unnamed,
                shape_error(
                    "a tuple struct",
                    "A wire key is a field name, and unnamed fields have none.",
                ),
            );
            None
        }
        syn::Fields::Unit => {
            errors.at(
                ident,
                shape_error(
                    "a unit struct",
                    "A wire key is a field name, and there are no fields at all — write \
                     `struct Marker {}` if an empty map is what you meant.",
                ),
            );
            None
        }
    }
}

/// The message every unsupported shape gets: the shape named, why it has no
/// schema, and the two ways out.
fn shape_error(shape: &str, because: &str) -> String {
    format!(
        "`#[derive(SharedState)]` describes a struct with named fields, and this is {shape}.\n\
         {because}\n\
         Fixes: give it named fields, or write `impl SharedState` for it by hand — a hand-written \
         impl is a supported escape hatch, not a workaround, and it is the documented way to \
         share a type the schema's own language cannot describe."
    )
}

/// Reads every field, dropping the ones that could not be understood.
fn read_fields(
    fields: &Punctuated<syn::Field, syn::token::Comma>,
    errors: &mut Errors,
) -> Vec<Field> {
    fields
        .iter()
        .filter_map(|field| read_field(field, errors))
        .collect()
}

/// Reads one field's attributes and settles its role.
fn read_field(field: &syn::Field, errors: &mut Errors) -> Option<Field> {
    let ident = field.ident.clone()?;
    let attrs = attr::Field::parse(&field.attrs, errors);
    let role = match (attrs.skip, &attrs.rename) {
        (Some(_), Some(rename)) => {
            errors.at(
                rename,
                "`#[duet(skip)]` and `#[duet(rename = \"...\")]` contradict each other: a skipped \
                 field has no wire key to rename.\n\
                 Fixes: drop the `rename` to keep the field out of the schema, or drop the `skip` \
                 to share it under that key.",
            );
            Role::Skipped
        }
        (Some(_), None) => Role::Skipped,
        (None, rename) => keys::role(&ident, rename.as_ref()),
    };
    Some(Field {
        ident,
        ty: field.ty.clone(),
        role,
    })
}

#[cfg(test)]
#[path = "model_tests.rs"]
mod tests;
