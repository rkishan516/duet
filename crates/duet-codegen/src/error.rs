//! Why a schema document could not be read, and why a schema could not be
//! emitted.

use std::fmt;

use duet_schema::SchemaErrors;

/// The most characters of an untrusted string any message here will echo.
///
/// A schema file is an input like any other: a hostile one can carry a
/// megabyte-long type name, and an error message that quoted it whole would
/// turn a rejection into a denial of service against whatever reads the log.
const ECHO_LIMIT: usize = 64;

/// `text`, cut to [`ECHO_LIMIT`] characters on a character boundary.
pub(crate) fn truncate(text: &str) -> String {
    match text.char_indices().nth(ECHO_LIMIT) {
        None => text.to_string(),
        Some((at, _)) => format!("{}…", &text[..at]),
    }
}

/// One reason a schema document is not a [`Schema`](duet_schema::Schema).
///
/// Every variant is a rejection of *input*: the document is read from a file
/// this crate does not control, so no shape of it may panic, allocate without
/// bound, or recurse without bound. Each variant carries where the problem was
/// found, spelled as a dotted trail from the document root.
#[derive(Debug)]
#[non_exhaustive]
pub enum ReadError {
    /// The bytes are not JSON at all, or nest deeper than `serde_json`'s
    /// default recursion limit.
    ///
    /// That limit is left **on**. `duet-protocol`'s corpus reader disables it
    /// for one file it owns; a schema file has no such need, and the limit is
    /// the cheapest bound on a hostile document there is.
    Json(serde_json::Error),
    /// A node is not the JSON kind the format requires there.
    WrongKind {
        /// Where, as a dotted trail from the document root.
        at: String,
        /// What the format requires: `"an object"`, `"a string"`, …
        expected: &'static str,
    },
    /// A required key is missing.
    MissingKey {
        /// Where the object was.
        at: String,
        /// The key the format requires.
        key: &'static str,
    },
    /// An object carries a key the format does not define.
    ///
    /// Rejected rather than ignored: a misspelled `"kind"` would otherwise read
    /// as "no kind at all" in some readers and "an unknown kind" in others, and
    /// a schema that means different things to different readers is worse than
    /// one that is refused.
    UnexpectedKey {
        /// Where the object was.
        at: String,
        /// The key nothing defines.
        key: String,
    },
    /// A `"kind"` naming no type in the schema's type language.
    UnknownKind {
        /// Where the type node was.
        at: String,
        /// The unrecognised spelling.
        kind: String,
    },
    /// The document declares a format version this reader does not know.
    UnsupportedVersion {
        /// What the document says.
        found: u64,
        /// What this reader implements.
        expected: u32,
    },
    /// A type node nests deeper than [`MAX_TY_DEPTH`](crate::MAX_TY_DEPTH).
    TooDeep {
        /// Where the nesting passed the limit.
        at: String,
        /// The limit.
        max: usize,
    },
    /// The document parsed, but the schema it describes is not valid.
    Invalid(SchemaErrors),
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReadError::Json(error) => write!(f, "the schema is not valid JSON: {error}"),
            ReadError::WrongKind { at, expected } => {
                write!(f, "{at} should be {expected}")
            }
            ReadError::MissingKey { at, key } => write!(f, "{at} has no \"{key}\""),
            ReadError::UnexpectedKey { at, key } => write!(
                f,
                "{at} carries \"{}\", which the schema format does not define",
                truncate(key)
            ),
            ReadError::UnknownKind { at, kind } => write!(
                f,
                "{at} has kind \"{}\", which is not a type this reader knows",
                truncate(kind)
            ),
            ReadError::UnsupportedVersion { found, expected } => write!(
                f,
                "the schema declares version {found}; this reader implements {expected}"
            ),
            ReadError::TooDeep { at, max } => {
                write!(f, "{at} nests more than {max} type constructors deep")
            }
            ReadError::Invalid(errors) => write!(f, "the schema is not valid: {errors}"),
        }
    }
}

impl std::error::Error for ReadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ReadError::Json(error) => Some(error),
            _ => None,
        }
    }
}

impl From<serde_json::Error> for ReadError {
    fn from(error: serde_json::Error) -> ReadError {
        ReadError::Json(error)
    }
}

/// One reason a valid schema still cannot be emitted as a typed client.
///
/// These are narrower than [`ReadError`]: the document is a well-formed,
/// validated [`Schema`](duet_schema::Schema), and the objection is that Dart
/// and TypeScript have no faithful spelling for some part of it. Every variant
/// names the fix, because the developer holding one owns the type definition
/// that caused it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum EmitError {
    /// The schema's root is not a named struct.
    ///
    /// A generated client is a class named after its root, and its accessors
    /// are that root's fields. A scalar root has neither, and needs no
    /// generated code: one hand-written `DuetField` at the empty path is the
    /// whole client.
    RootNotNamed {
        /// How the root was spelled, for the message.
        found: String,
    },
    /// An `optional` appears somewhere other than as a struct field's own type.
    ///
    /// `DuetCodec`'s type argument is non-nullable **by design** — that bound
    /// is what keeps "decoded to null" from colliding with "refused" — so
    /// there is no codec for an optional list element or an optional map
    /// value, and an `Option<Option<T>>` has no second place to put its
    /// nullability either. Lift the option to the field, or give the inner
    /// type a struct of its own.
    MisplacedOptional {
        /// The type holding the field.
        type_name: String,
        /// The field's wire key.
        key: String,
    },
    /// A wire key cannot be spelled as an accessor in Dart and TypeScript.
    ///
    /// The key must survive three separate journeys: as a path segment on the
    /// wire, as a fragment of a generated string literal, and as an identifier.
    /// The narrowest of the three wins.
    UnspellableKey {
        /// The type holding the field.
        type_name: String,
        /// The offending key.
        key: String,
        /// What is wrong with it.
        because: &'static str,
    },
    /// Two of one type's fields want the same accessor name.
    ///
    /// `foo_bar` and `fooBar` are different wire keys and the same Dart
    /// identifier. One of them must be renamed.
    AccessorCollision {
        /// The type holding both fields.
        type_name: String,
        /// The contested accessor name.
        accessor: String,
    },
    /// Two generated declarations want the same name.
    ///
    /// A schema type called `AppEditor` and the accessor class for `App`'s
    /// `editor` field would both claim `AppEditor`, and one would silently win.
    DeclarationCollision {
        /// The contested name.
        name: String,
    },
    /// The schema reaches more struct-typed paths than this emitter will
    /// expand.
    ///
    /// Every struct-typed path gets an accessor class holding one literal per
    /// field, so a schema shaped like a diamond expands combinatorially. The
    /// cap is what keeps a valid but pathological schema from turning into an
    /// unbounded amount of generated source.
    TooManyClasses {
        /// The cap.
        max: usize,
    },
    /// A [`Ty`](duet_schema::Ty) arm this emitter has no spelling for.
    ///
    /// `Ty` is `#[non_exhaustive]`; Phase 4b adds arms. Reaching one here is a
    /// missing emitter update, not a bad schema.
    UnsupportedType {
        /// The type holding the field, or the root.
        type_name: String,
        /// The field's wire key, or `""` at the root.
        key: String,
    },
}

impl fmt::Display for EmitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EmitError::RootNotNamed { found } => write!(
                f,
                "the schema's root is {}, but a generated client needs a named \
                 struct at the root to take its name and its fields from",
                truncate(found)
            ),
            EmitError::MisplacedOptional { type_name, key } => write!(
                f,
                "field \"{}\" of type \"{}\" puts an optional inside a container \
                 or inside another optional; a codec's type argument is \
                 non-nullable, so lift the option to the field itself",
                truncate(key),
                truncate(type_name)
            ),
            EmitError::UnspellableKey {
                type_name,
                key,
                because,
            } => write!(
                f,
                "key \"{}\" on type \"{}\" cannot be a generated accessor: {because}",
                truncate(key),
                truncate(type_name)
            ),
            EmitError::AccessorCollision {
                type_name,
                accessor,
            } => write!(
                f,
                "two fields of type \"{}\" both want the accessor \"{}\"; rename one",
                truncate(type_name),
                truncate(accessor)
            ),
            EmitError::DeclarationCollision { name } => write!(
                f,
                "two generated declarations both want the name \"{}\"; rename the \
                 schema type that clashes with the accessor class",
                truncate(name)
            ),
            EmitError::TooManyClasses { max } => write!(
                f,
                "the schema reaches more than {max} struct-typed paths; \
                 expanding them all would generate an unbounded amount of source"
            ),
            EmitError::UnsupportedType { type_name, key } => write!(
                f,
                "field \"{}\" of type \"{}\" has a schema type this emitter has \
                 no spelling for; the emitter needs updating",
                truncate(key),
                truncate(type_name)
            ),
        }
    }
}

impl std::error::Error for EmitError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_echo_is_cut_on_a_character_boundary() {
        let long = "é".repeat(200);
        let cut = truncate(&long);
        assert!(cut.ends_with('…'));
        assert_eq!(cut.chars().count(), ECHO_LIMIT + 1);
    }

    #[test]
    fn a_short_echo_is_left_alone() {
        assert_eq!(truncate("zoom"), "zoom");
    }

    #[test]
    fn every_read_error_says_something_specific() {
        let messages = [
            ReadError::WrongKind {
                at: "root".to_string(),
                expected: "an object",
            }
            .to_string(),
            ReadError::MissingKey {
                at: "root".to_string(),
                key: "kind",
            }
            .to_string(),
            ReadError::UnexpectedKey {
                at: "root".to_string(),
                key: "knid".to_string(),
            }
            .to_string(),
            ReadError::UnknownKind {
                at: "root".to_string(),
                kind: "u64".to_string(),
            }
            .to_string(),
            ReadError::UnsupportedVersion {
                found: 9,
                expected: 1,
            }
            .to_string(),
            ReadError::TooDeep {
                at: "root".to_string(),
                max: 64,
            }
            .to_string(),
        ];
        for message in &messages {
            assert!(!message.is_empty());
        }
        assert!(messages[2].contains("knid"));
        assert!(messages[3].contains("u64"));
    }

    #[test]
    fn a_json_error_is_reported_with_its_source() {
        let error: ReadError = serde_json::from_str::<serde_json::Value>("{")
            .expect_err("not JSON")
            .into();
        assert!(error.to_string().contains("not valid JSON"));
        assert!(std::error::Error::source(&error).is_some());
    }

    #[test]
    fn a_schema_error_is_reported_without_a_source() {
        let error = ReadError::TooDeep {
            at: "root".to_string(),
            max: 4,
        };
        assert!(std::error::Error::source(&error).is_none());
    }

    #[test]
    fn every_emit_error_names_the_field_it_objects_to() {
        let errors = [
            EmitError::RootNotNamed {
                found: "int".to_string(),
            },
            EmitError::MisplacedOptional {
                type_name: "App".to_string(),
                key: "tags".to_string(),
            },
            EmitError::UnspellableKey {
                type_name: "App".to_string(),
                key: "a b".to_string(),
                because: "it is not an ASCII identifier",
            },
            EmitError::AccessorCollision {
                type_name: "App".to_string(),
                accessor: "fooBar".to_string(),
            },
            EmitError::DeclarationCollision {
                name: "AppEditor".to_string(),
            },
            EmitError::TooManyClasses { max: 256 },
            EmitError::UnsupportedType {
                type_name: "App".to_string(),
                key: "future".to_string(),
            },
        ];
        for error in &errors {
            assert!(!error.to_string().is_empty(), "{error:?}");
        }
        assert!(errors[1].to_string().contains("tags"));
        assert!(errors[2].to_string().contains("ASCII identifier"));
        assert!(errors[5].to_string().contains("256"));
    }
}
