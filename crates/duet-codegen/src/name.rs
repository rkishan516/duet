//! Turning wire keys into identifiers — and, deliberately, never the reverse.
//!
//! # The rule, stated once
//!
//! **A wire key is never rewritten. An accessor name always is.**
//!
//! A Rust `snake_case` field becomes a `lowerCamelCase` accessor in Dart and in
//! TypeScript, because that is what a developer in either language expects to
//! type. The **path** the accessor addresses keeps the key exactly as the
//! schema spells it.
//!
//! That asymmetry is the whole point. The path is the *wire*: it is the key in
//! a `Value::Map` on the host, and it is what a second guest uses to address the
//! same node. Camel-casing it would mean a Dart guest writing `editor.fontSize`
//! and a Rust host owning `editor.font_size` — two names for what the developer
//! believes is one field, no error anywhere, and each guest reading only its own
//! writes. `duet-codegen` mints `'editor.font_size'` and calls the accessor
//! `fontSize`.
//!
//! `tests/casing.rs` pins this from both directions, and `tests/real_host.rs`
//! takes every path literal out of the committed golden files and resolves it
//! against a real store — so a camel-cased path fails against the host, not
//! merely against an assertion.
//!
//! # Why identifiers are ASCII-only
//!
//! [`is_legal_key`](duet_schema) admits far more than this module does: `café`
//! and `with space` are legal wire keys. A generated accessor has to be a Dart
//! identifier (ASCII only) *and* a TypeScript one *and* survive being spelled
//! inside a string literal with no escaping. The narrowest of those three wins,
//! and the emitter refuses the rest by name rather than mangling them silently.

/// The Dart words that cannot be an identifier, plus the ones that cannot be
/// one *here*.
///
/// Three groups, and the reason for each:
///
/// - Dart's reserved words, which are a syntax error as a member name.
/// - Dart's built-in and contextual identifiers (`dynamic`, `covariant`,
///   `await`, …). Legal in some positions and not others; the emitter does not
///   track which position it is in, so it refuses them all.
/// - `hashCode`, `runtimeType`, `toString`, `noSuchMethod` — every Dart class
///   already has these, and an accessor with one of those names would be an
///   invalid override rather than a new member.
///
/// TypeScript would accept nearly all of these as property names. They are
/// refused for both languages anyway, because **one** naming rule that holds in
/// both is worth more than two rules that agree most of the time: a schema must
/// not generate a Dart client and a TypeScript client with different accessor
/// names.
const RESERVED: &[&str] = &[
    "abstract",
    "as",
    "assert",
    "async",
    "await",
    "base",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "covariant",
    "default",
    "deferred",
    "do",
    "dynamic",
    "else",
    "enum",
    "export",
    "extends",
    "extension",
    "external",
    "factory",
    "false",
    "final",
    "finally",
    "for",
    "function",
    "get",
    "hashCode",
    "hide",
    "if",
    "implements",
    "import",
    "in",
    "interface",
    "is",
    "late",
    "library",
    "mixin",
    "new",
    "noSuchMethod",
    "null",
    "on",
    "operator",
    "part",
    "required",
    "rethrow",
    "return",
    "runtimeType",
    "sealed",
    "set",
    "show",
    "static",
    "super",
    "switch",
    "sync",
    "this",
    "throw",
    "toString",
    "true",
    "try",
    "typedef",
    "var",
    "void",
    "when",
    "while",
    "with",
    "yield",
];

/// The names the emitter puts on every generated accessor class itself.
///
/// A field whose accessor collided with one of these would shadow the router
/// the class needs, or the handle on the struct as a whole.
const EMITTER_OWNED: &[&str] = &["router", "self"];

/// The name the generated **commands** class puts on itself.
///
/// Separate from [`EMITTER_OWNED`] because it applies to a different namespace:
/// a schema *field* called `client` is perfectly emittable, and refusing it
/// would be a rule imposed on state by a feature state has nothing to do with.
/// Inside the commands class, though, `client` is both the field every method
/// body reads and — in Dart, where a named parameter shadows a field for the
/// whole body — a name a parameter would silently steal. So it is refused for
/// command method names and for parameter names alike, in both languages, so
/// that one schema cannot generate two clients with different spellings.
const COMMANDS_OWNED: &[&str] = &["client"];

/// True if `key` may be spelled inside a generated path literal with no
/// escaping and no ambiguity.
///
/// Deliberately narrower than the wire's own key grammar. A key outside this
/// set is refused by name rather than escaped, because an escaped key would
/// make the golden files stop being greppable for the exact wire string — which
/// is one of the three things the compile-time-literal rule buys.
pub fn is_emittable_key(key: &str) -> bool {
    !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// True if `name` is a legal identifier in Dart and TypeScript both, and is not
/// one this emitter has already claimed.
pub fn is_usable_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_ascii_alphabetic()
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !RESERVED.contains(&name)
        && !EMITTER_OWNED.contains(&name)
}

/// True if `name` may be a member of the generated commands class — a method
/// name or a parameter name.
///
/// [`is_usable_identifier`] plus the one name the generated commands class puts
/// on itself, `client`. The two sets are separate because they apply to
/// different namespaces: a schema *field* called `client` is perfectly
/// emittable, and refusing it would impose a rule on state that only commands
/// need.
pub fn is_usable_member(name: &str) -> bool {
    is_usable_identifier(name) && !COMMANDS_OWNED.contains(&name)
}

/// A dotted command name as one `lowerCamelCase` identifier.
///
/// `documents.close` becomes `documentsClose`, exactly as `documents_close`
/// would: a dot separates in the same way an underscore does, and a name that
/// used both must not land on two different identifiers. The **wire** name is
/// untouched, here as everywhere — see this module's opening rule.
pub fn command_method(name: &str) -> String {
    lower_camel(&name.replace('.', "_"))
}

/// `snake_case` to `lowerCamelCase`, the accessor spelling both target
/// languages share.
///
/// Underscores are separators and disappear; a key that was already
/// `lowerCamel` comes back unchanged. `foo_bar` and `fooBar` therefore land on
/// the same accessor, which is a collision the emitter reports rather than
/// resolves — nothing here can know which of the two the developer meant.
pub fn lower_camel(key: &str) -> String {
    let mut out = String::with_capacity(key.len());
    for (position, part) in key.split('_').filter(|p| !p.is_empty()).enumerate() {
        if position == 0 {
            push_with_first(&mut out, part, char::to_ascii_lowercase);
        } else {
            push_with_first(&mut out, part, char::to_ascii_uppercase);
        }
    }
    out
}

/// `snake_case` to `UpperCamelCase`, for the names of generated classes.
pub fn upper_camel(key: &str) -> String {
    let mut out = String::with_capacity(key.len());
    for part in key.split('_').filter(|p| !p.is_empty()) {
        push_with_first(&mut out, part, char::to_ascii_uppercase);
    }
    out
}

/// Appends `part` with `first` applied to its first character only.
///
/// The rest is left alone rather than lowercased, so a key that already carries
/// a deliberate spelling — `httpURL` — keeps it instead of being flattened into
/// something the developer did not write.
fn push_with_first(out: &mut String, part: &str, first: fn(&char) -> char) {
    let mut chars = part.chars();
    if let Some(head) = chars.next() {
        out.push(first(&head));
        out.extend(chars);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snake_case_becomes_lower_camel() {
        assert_eq!(lower_camel("counter"), "counter");
        assert_eq!(lower_camel("font_size"), "fontSize");
        assert_eq!(lower_camel("a_b_c"), "aBC");
        assert_eq!(lower_camel("nested_editor_state"), "nestedEditorState");
    }

    #[test]
    fn an_already_camel_key_is_left_alone() {
        assert_eq!(lower_camel("fontSize"), "fontSize");
        assert_eq!(lower_camel("httpURL"), "httpURL");
    }

    #[test]
    fn leading_and_repeated_underscores_collapse() {
        // Deliberately lossy: `_x` and `x` both become `x`, and the emitter
        // reports the collision rather than inventing a distinction.
        assert_eq!(lower_camel("_private"), "private");
        assert_eq!(lower_camel("a__b"), "aB");
        assert_eq!(lower_camel("trailing_"), "trailing");
    }

    #[test]
    fn upper_camel_capitalises_every_part() {
        assert_eq!(upper_camel("editor"), "Editor");
        assert_eq!(upper_camel("font_size"), "FontSize");
        assert_eq!(upper_camel("a_b_c"), "ABC");
    }

    #[test]
    fn an_emittable_key_is_ascii_alphanumeric_or_underscore() {
        for good in ["a", "zoom", "font_size", "_private", "x2", "3"] {
            assert!(is_emittable_key(good), "{good} should be emittable");
        }
        for bad in ["", "with space", "café", "a-b", "a'b", "a\"b", "a\nb"] {
            assert!(!is_emittable_key(bad), "{bad} should be refused");
        }
    }

    #[test]
    fn a_usable_identifier_starts_with_a_letter_and_is_not_reserved() {
        for good in ["a", "zoom", "fontSize", "x2", "a_b"] {
            assert!(is_usable_identifier(good), "{good} should be usable");
        }
        for bad in ["", "2x", "_x", "with space", "café"] {
            assert!(!is_usable_identifier(bad), "{bad} should be refused");
        }
    }

    #[test]
    fn dart_reserved_words_are_refused() {
        for reserved in [
            "class", "const", "default", "is", "void", "dynamic", "final",
        ] {
            assert!(
                !is_usable_identifier(reserved),
                "{reserved} is a Dart reserved word"
            );
        }
    }

    #[test]
    fn object_members_are_refused_because_an_accessor_would_be_a_bad_override() {
        for member in ["hashCode", "runtimeType", "toString", "noSuchMethod"] {
            assert!(!is_usable_identifier(member), "{member} is on Object");
        }
    }

    #[test]
    fn the_names_the_emitter_owns_are_refused() {
        for owned in ["router", "self"] {
            assert!(
                !is_usable_identifier(owned),
                "{owned} is the emitter's own member"
            );
        }
    }

    #[test]
    fn the_reserved_list_is_sorted_and_free_of_repeats() {
        // Sorted so a reader can find a word, and repeat-free so removing one
        // entry actually removes the rejection.
        let mut sorted = RESERVED.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.as_slice(), RESERVED);
    }
}
