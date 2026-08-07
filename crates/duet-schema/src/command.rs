//! Commands as the schema describes them: [`CommandDef`].
//!
//! A guest reads and writes state through paths, and it asks the host to *do
//! things* through `Request::Invoke`. The first half
//! has had a schema since Phase 4; this is the second half, so a generated
//! client can offer `add(a, b)` with types instead of a stringly-typed `invoke`.
//!
//! # What a command definition carries, and what it deliberately does not
//!
//! It carries the name, the parameters, and the two types the wire can answer
//! with. It does **not** carry documentation, deprecation, a permission, or a
//! surface. Authorization in Duet is by construction — a surface reaches the
//! commands the embedder built it with, and nothing else — so a permission in
//! the schema would be a claim about a decision made somewhere the schema
//! cannot see. See `duet_protocol::CommandHost`.

use crate::ty::{FieldDef, Ty, is_legal_type_name};

/// One command, as the schema describes it.
///
/// # Why `returns` and `raises` are separate, and both optional
///
/// The wire already distinguishes a `returned` from a `raised`: a command that
/// *ran and failed* is a different event from one that could not be run at all,
/// and a guest that cannot tell them apart cannot decide whether retrying is
/// safe. A schema carrying only one type would force a generated client to
/// decode both replies through it, which is wrong for exactly the case the
/// distinction exists for.
///
/// Both are `Option` because both are genuinely absent for some commands, and
/// the schema's type language has no spelling for "nothing":
///
/// | Rust | `returns` | `raises` |
/// |---|---|---|
/// | `-> i64` | `int` | absent |
/// | `-> ()` (or no `->`) | absent | absent |
/// | `-> Result<i64, E>` | `int` | `E` |
/// | `-> Result<(), E>` | absent | `E` |
///
/// Absence is spelled by **omitting the key**, not by a JSON `null` and not by
/// a `{"kind": "unit"}`. A [`Ty`] describes a value that can exist in the store,
/// and inventing an arm for "no value" would make it expressible as a struct
/// field, where it would mean nothing at all and every emitter would have to
/// have an opinion about it.
///
/// A command with no `returns` still answers: the wire carries
/// [`Value::Null`](duet_core::Value::Null), which is what
/// `Outcome::Returned` documents for a command with
/// no result. `returns` describes the *type*, and there is none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandDef {
    /// The name a guest invokes, unique across the schema.
    pub name: String,
    /// Its parameters, in **declaration order**.
    ///
    /// Declaration order rather than sorted, for the same reason
    /// [`TypeDef`](crate::TypeDef)'s fields are: a generated client takes them
    /// as positional arguments, so reordering them is a source-breaking change
    /// for every guest and must be a deliberate edit to the Rust function
    /// rather than an emergent property of a map's iteration order.
    ///
    /// A [`FieldDef`] rather than a type of its own: a parameter is a key and a
    /// type, which is exactly what a field is, and it occupies a key in the
    /// same `args` map a struct's field occupies in a `Value::Map`. Two names
    /// for one shape would be two things to keep in step.
    pub params: Vec<FieldDef>,
    /// What a `returned` reply carries, or `None` for a command with no result.
    pub returns: Option<Ty>,
    /// What a `raised` reply carries, or `None` for a command that cannot fail.
    pub raises: Option<Ty>,
}

impl CommandDef {
    /// Describes a command that returns a value and cannot raise.
    ///
    /// The two-thirds of the table above that need no `Option` at the call
    /// site; a fallible command builds the struct literally.
    pub fn new(name: impl Into<String>, params: Vec<FieldDef>, returns: Ty) -> CommandDef {
        CommandDef {
            name: name.into(),
            params,
            returns: Some(returns),
            raises: None,
        }
    }

    /// Every type name this command mentions, in the order they appear.
    ///
    /// Parameters first, then the return, then the error — the order the
    /// definition is written and the order it is rendered in.
    pub(crate) fn referenced_names(&self) -> Vec<&str> {
        let mut found = Vec::new();
        for param in &self.params {
            found.extend(param.ty.referenced_names());
        }
        for ty in [&self.returns, &self.raises].into_iter().flatten() {
            found.extend(ty.referenced_names());
        }
        found
    }

    /// Every [`Ty`] this command declares, for a depth walk.
    pub(crate) fn declared_types(&self) -> impl Iterator<Item = &Ty> {
        self.params
            .iter()
            .map(|param| &param.ty)
            .chain(self.returns.iter())
            .chain(self.raises.iter())
    }
}

/// True if `name` is a legal command name.
///
/// A dot-separated sequence of legal identifiers, so `subtract` and
/// `documents.rename` are both names and `.rename`, `documents.` and
/// `documents..rename` are not. The dot is admitted because namespacing is how
/// a registry of any size stays readable, and it is the one separator already
/// meaningful to a reader of this project's own command tables.
///
/// The per-segment rule is [`is_legal_type_name`] rather than a second grammar:
/// a segment has to survive as an identifier in Dart and TypeScript exactly as
/// a type name does, and one predicate that is right in two places is better
/// than two that can drift apart.
pub(crate) fn is_legal_command_name(name: &str) -> bool {
    !name.is_empty() && name.split('.').all(is_legal_type_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legal_command_names_are_dotted_identifiers() {
        for good in [
            "add",
            "subtract",
            "documents.rename",
            "a.b.c",
            "reset_counter",
            "Bump2",
        ] {
            assert!(is_legal_command_name(good), "{good} should be legal");
        }
    }

    #[test]
    fn illegal_command_names_are_rejected() {
        for bad in [
            "",
            ".",
            ".rename",
            "documents.",
            "documents..rename",
            "2fast",
            "has space",
            "has-dash",
            "trailing_",
            "_leading",
            "café",
        ] {
            assert!(!is_legal_command_name(bad), "{bad} should be rejected");
        }
    }

    #[test]
    fn a_command_names_every_type_it_mentions_in_written_order() {
        let command = CommandDef {
            name: "save".to_string(),
            params: vec![FieldDef::new("doc", Ty::Named("Doc".into()))],
            returns: Some(Ty::Named("Receipt".into()).list()),
            raises: Some(Ty::Named("SaveError".into())),
        };
        assert_eq!(
            command.referenced_names(),
            ["Doc", "Receipt", "SaveError"],
            "parameters, then the return, then the error"
        );
        assert_eq!(command.declared_types().count(), 3);
    }

    #[test]
    fn a_command_with_neither_a_return_nor_an_error_mentions_nothing() {
        let command = CommandDef {
            name: "reset".to_string(),
            params: Vec::new(),
            returns: None,
            raises: None,
        };
        assert!(command.referenced_names().is_empty());
        assert_eq!(command.declared_types().count(), 0);
    }

    #[test]
    fn the_constructor_describes_an_infallible_command() {
        let command = CommandDef::new("add", vec![FieldDef::new("a", Ty::Int)], Ty::Int);
        assert_eq!(command.returns, Some(Ty::Int));
        assert_eq!(command.raises, None);
    }
}
