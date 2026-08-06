//! The three `#[duet(...)]` attributes, and nothing else.
//!
//! # Why exactly three
//!
//! | Attribute | Position | What it changes |
//! |---|---|---|
//! | `#[duet(rename = "window_title")]` | field | the wire key |
//! | `#[duet(crate = ::my_reexport)]` | container | the path the generated code names Duet by |
//! | `#[duet(skip)]` | field | the field is not shared state at all |
//!
//! There is deliberately **no `#[duet(with = ...)]`**. The escape hatch for a
//! type Duet does not accept is a hand-written `impl SharedState`, which is
//! strictly more powerful — it can say what `to_value`, `from_value` *and*
//! `schema` do, and the compiler checks the three against one signature. A
//! `with` attribute would name a module the macro cannot see inside, so nothing
//! would check that the codec it supplies and the schema it claims agree; the
//! failure would be a Dart client generated for a shape the host never writes.
//!
//! An unrecognised key is an error rather than a silent no-op, because a typo
//! in `#[duet(renmae = "...")]` that compiled would ship the wrong wire key.

use proc_macro2::Span;
use syn::meta::ParseNestedMeta;
use syn::spanned::Spanned as _;

use crate::errors::Errors;

/// The attribute name every Duet attribute lives under.
pub const NAMESPACE: &str = "duet";

/// What the three attributes are called, for a diagnostic that lists them.
const KNOWN: &str = "`rename = \"...\"`, `crate = ::path` or `skip`";

/// `#[duet(...)]` on the struct itself.
#[derive(Default)]
pub struct Container {
    /// The path the generated code names Duet by, when `crate = ...` was given.
    pub krate: Option<syn::Path>,
}

/// `#[duet(...)]` on one field.
#[derive(Default)]
pub struct Field {
    /// The wire key this field was renamed onto.
    pub rename: Option<syn::LitStr>,
    /// Where `skip` was written, when it was.
    pub skip: Option<Span>,
}

impl Container {
    /// Reads every `#[duet(...)]` on a struct definition.
    ///
    /// Problems are accumulated rather than returned, so one bad attribute does
    /// not hide the next.
    pub fn parse(attrs: &[syn::Attribute], errors: &mut Errors) -> Container {
        let mut found = Container::default();
        for_each_meta(attrs, errors, |meta, errors| {
            if meta.path.is_ident("crate") {
                set_once(
                    &mut found.krate,
                    meta.value()?.parse()?,
                    "crate",
                    meta,
                    errors,
                );
            } else if meta.path.is_ident("rename") || meta.path.is_ident("skip") {
                let name = ident_of(&meta.path);
                errors.push(meta.error(format!(
                    "`{name}` describes one field, so it belongs on a field rather than on the \
                     struct — move it to the field it applies to"
                )));
                swallow(&meta);
            } else {
                errors.push(unknown(&meta));
                swallow(&meta);
            }
            Ok(())
        });
        found
    }
}

impl Field {
    /// Reads every `#[duet(...)]` on one field.
    pub fn parse(attrs: &[syn::Attribute], errors: &mut Errors) -> Field {
        let mut found = Field::default();
        for_each_meta(attrs, errors, |meta, errors| {
            if meta.path.is_ident("rename") {
                set_once(
                    &mut found.rename,
                    meta.value()?.parse()?,
                    "rename",
                    meta,
                    errors,
                );
            } else if meta.path.is_ident("skip") {
                set_once(&mut found.skip, meta.path.span(), "skip", meta, errors);
            } else if meta.path.is_ident("crate") {
                errors.push(meta.error(
                    "`crate` names the path the whole generated impl uses, so it belongs on the \
                     struct rather than on a field — move it up to the `#[derive(SharedState)]` \
                     type",
                ));
                swallow(&meta);
            } else {
                errors.push(unknown(&meta));
                swallow(&meta);
            }
            Ok(())
        });
        found
    }
}

/// Runs `each` over every meta item inside every `#[duet(...)]` in `attrs`.
///
/// A parse failure inside one attribute is recorded and the next attribute is
/// still read: `syn`'s nested-meta parser stops at the first error within one
/// attribute, and stopping across all of them as well would report one problem
/// per compile.
fn for_each_meta(
    attrs: &[syn::Attribute],
    errors: &mut Errors,
    mut each: impl FnMut(ParseNestedMeta, &mut Errors) -> syn::Result<()>,
) {
    for attr in attrs.iter().filter(|a| a.path().is_ident(NAMESPACE)) {
        if let syn::Meta::Path(path) = &attr.meta {
            errors.at(
                path,
                format!("`#[duet]` says nothing on its own — write `#[duet(...)]` with {KNOWN}"),
            );
            continue;
        }
        if let Err(error) = attr.parse_nested_meta(|meta| each(meta, errors)) {
            errors.push(error);
        }
    }
}

/// Stores `value` in `slot`, or reports that the key was already given.
///
/// Two `#[duet(rename = ...)]`s on one field are not a merge and not a
/// last-one-wins: they are a developer who edited one and forgot the other, and
/// silently keeping either is how the wrong wire key ships.
fn set_once<T>(
    slot: &mut Option<T>,
    value: T,
    name: &str,
    meta: ParseNestedMeta,
    errors: &mut Errors,
) {
    if slot.is_some() {
        errors.push(meta.error(format!(
            "`{name}` is given twice, and only one of them can apply — delete the one that is \
             wrong rather than leaving both"
        )));
        return;
    }
    *slot = Some(value);
}

/// The rejection for a key that is not one of the three.
fn unknown(meta: &ParseNestedMeta) -> syn::Error {
    let name = ident_of(&meta.path);
    meta.error(format!(
        "`{name}` is not a Duet attribute — the three are {KNOWN}"
    ))
}

/// Discards whatever is left inside this `#[duet(...)]`.
///
/// Called after a key has already been refused. Without it, `syn`'s
/// nested-meta loop would go on to demand a `,` where the rejected key's `= "x"`
/// still sits, and the developer would get a bare "expected `,`" *alongside*
/// the message that says what is actually wrong — two complaints for one edit,
/// one of them noise.
fn swallow(meta: &ParseNestedMeta) {
    let _ = meta.input.parse::<proc_macro2::TokenStream>();
}

/// A path's last segment, for a message. `crate` and friends are keywords, so
/// this cannot go through `Ident::to_string` on the path as a whole.
fn ident_of(path: &syn::Path) -> String {
    path.segments
        .last()
        .map(|segment| segment.ident.to_string())
        .unwrap_or_default()
}

#[cfg(test)]
#[path = "attr_tests.rs"]
mod tests;
