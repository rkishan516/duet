//! The two `#[command(...)]` keys, and the one `#[duet(...)]` key a parameter
//! may carry.
//!
//! # Why two, not three
//!
//! | Attribute | Position | What it changes |
//! |---|---|---|
//! | `#[command(rename = "documents.add")]` | function | the name a guest invokes |
//! | `#[command(crate = ::my_reexport)]` | function | the path the generated code names Duet by |
//! | `#[duet(rename = "by")]` | parameter | the argument key |
//!
//! `#[derive(SharedState)]` has a third, `skip`, and it has no meaning here. A
//! skipped *field* is state the store does not hold, and the struct still
//! decodes because `Default` fills it in. A skipped *parameter* would be an
//! argument the guest cannot supply and the body still requires, and there is
//! nothing to fill it from — a command's arguments are the whole of its input.
//! It is refused with a message saying so rather than silently ignored.
//!
//! A parameter's rename lives under `#[duet(...)]` rather than
//! `#[command(...)]` because `#[command]` is the attribute macro itself: a
//! second `#[command(...)]` inside the signature would read as a nested
//! invocation. `#[duet(...)]` is the same inert namespace the derive uses for
//! the same job.

use proc_macro2::TokenStream;
use syn::meta::ParseNestedMeta;
use syn::parse::Parser as _;

use crate::attr::NAMESPACE;
use crate::errors::Errors;

/// What the two keys are called, for a diagnostic that lists them.
const KNOWN: &str = "`rename = \"...\"` or `crate = ::path`";

/// `#[command(...)]` on the function.
#[derive(Default)]
pub struct Command {
    /// The name a guest invokes, when `rename = ...` was given.
    pub rename: Option<syn::LitStr>,
    /// The path the generated code names Duet by, when `crate = ...` was given.
    pub krate: Option<syn::Path>,
}

/// `#[duet(...)]` on one parameter.
#[derive(Default)]
pub struct Param {
    /// The argument key this parameter was renamed onto.
    pub rename: Option<syn::LitStr>,
}

impl Command {
    /// Reads the tokens inside `#[command(...)]`.
    ///
    /// Problems are accumulated rather than returned, so one bad key does not
    /// hide the next.
    pub fn parse(attr: TokenStream, errors: &mut Errors) -> Command {
        let mut found = Command::default();
        let parser = syn::meta::parser(|meta| {
            if meta.path.is_ident("rename") {
                set_once(
                    &mut found.rename,
                    meta.value()?.parse()?,
                    "rename",
                    &meta,
                    errors,
                );
            } else if meta.path.is_ident("crate") {
                set_once(
                    &mut found.krate,
                    meta.value()?.parse()?,
                    "crate",
                    &meta,
                    errors,
                );
            } else if meta.path.is_ident("skip") {
                errors.push(meta.error(
                    "`skip` has no meaning on a command: a command's arguments are the whole of \
                     its input, so a skipped one would be an argument the guest cannot supply and \
                     the body still requires.\n\
                     Fixes: delete the parameter, or give it a default by taking `Option<T>` and \
                     deciding in the body.",
                ));
                swallow(&meta);
            } else {
                errors.push(unknown(&meta));
                swallow(&meta);
            }
            Ok(())
        });
        if let Err(error) = parser.parse2(attr) {
            errors.push(error);
        }
        found
    }
}

impl Param {
    /// Reads every `#[duet(...)]` on one parameter.
    pub fn parse(attrs: &[syn::Attribute], errors: &mut Errors) -> Param {
        let mut found = Param::default();
        for attr in attrs.iter().filter(|a| a.path().is_ident(NAMESPACE)) {
            if let syn::Meta::Path(path) = &attr.meta {
                errors.at(
                    path,
                    "`#[duet]` says nothing on its own — write `#[duet(rename = \"...\")]`",
                );
                continue;
            }
            if let Err(error) = attr.parse_nested_meta(|meta| {
                read_param_key(&mut found, &meta, errors);
                Ok(())
            }) {
                errors.push(error);
            }
        }
        found
    }
}

/// One key inside a parameter's `#[duet(...)]`.
fn read_param_key(found: &mut Param, meta: &ParseNestedMeta, errors: &mut Errors) {
    if meta.path.is_ident("rename") {
        match meta.value().and_then(syn::parse::ParseBuffer::parse) {
            Ok(literal) => set_once(&mut found.rename, literal, "rename", meta, errors),
            Err(error) => errors.push(error),
        }
    } else if meta.path.is_ident("crate") {
        errors.push(meta.error(
            "`crate` names the path the whole generated impl uses, so it belongs on the function \
             rather than on a parameter — write `#[command(crate = ::path)]`",
        ));
        swallow(meta);
    } else {
        errors.push(meta.error(format!(
            "`{}` is not a Duet parameter attribute — the only one is `rename = \"...\"`",
            ident_of(&meta.path)
        )));
        swallow(meta);
    }
}

/// Stores `value` in `slot`, or reports that the key was already given.
///
/// Two `rename`s are not a merge and not a last-one-wins: they are a developer
/// who edited one and forgot the other, and silently keeping either is how the
/// wrong name ships.
fn set_once<T>(
    slot: &mut Option<T>,
    value: T,
    name: &str,
    meta: &ParseNestedMeta,
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

/// The rejection for a key that is not one of the two.
fn unknown(meta: &ParseNestedMeta) -> syn::Error {
    meta.error(format!(
        "`{}` is not a `#[command]` attribute — the two are {KNOWN}",
        ident_of(&meta.path)
    ))
}

/// Discards whatever is left inside this attribute.
///
/// Called after a key has already been refused. Without it, `syn`'s nested-meta
/// loop goes on to demand a `,` where the rejected key's `= "x"` still sits, and
/// the developer gets a bare "expected `,`" *alongside* the message that says
/// what is actually wrong — two complaints for one edit, one of them noise.
fn swallow(meta: &ParseNestedMeta) {
    let _ = meta.input.parse::<TokenStream>();
}

/// A path's last segment, for a message. `crate` is a keyword, so this cannot
/// go through `Ident::to_string` on the path as a whole.
fn ident_of(path: &syn::Path) -> String {
    path.segments
        .last()
        .map(|segment| segment.ident.to_string())
        .unwrap_or_default()
}

/// Where to point a complaint about a parameter's key.
pub fn key_span(rename: Option<&syn::LitStr>, ident: &syn::Ident) -> proc_macro2::Span {
    rename.map_or_else(|| ident.span(), syn::spanned::Spanned::span)
}

#[cfg(test)]
#[path = "attr_tests.rs"]
mod tests;
