//! Why a set of [`SharedState`](crate::SharedState) impls is not a schema.

use crate::echo;

/// One reason a schema could not be built.
///
/// Every variant is a *definition* problem, found once at startup, not a
/// runtime condition. They are separated from
/// [`DecodeError`](crate::DecodeError) — which is about hostile data — because
/// the audiences are different: this is a developer bug in the type
/// definitions, and it should stop the process before a guest ever connects.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SchemaError {
    /// A type reaches itself.
    ///
    /// Carries the chain from the outermost type in the cycle back to itself,
    /// e.g. `["Node", "Node"]` or `["A", "B", "A"]`.
    ///
    /// Recursive shared state is unrepresentable, not merely unimplemented:
    /// the store is a finite tree bounded at
    /// [`MAX_VALUE_DEPTH`](duet_core::MAX_VALUE_DEPTH), and a self-referential
    /// type has no finite set of paths for a client to address.
    Recursive {
        /// The cycle, first element repeated as the last.
        chain: Vec<String>,
    },
    /// Two different Rust types claimed the same schema name.
    ///
    /// One would silently win and every generated client would be built
    /// against the wrong shape. Rename one of them, or give one an explicit
    /// schema name.
    NameCollision {
        /// The contested name.
        name: String,
    },
    /// Two fields of one type map onto the same wire key.
    ///
    /// They would occupy one map entry: one of the two becomes unreadable, and
    /// which one is not something the developer chose.
    DuplicateKey {
        /// The type holding both fields.
        type_name: String,
        /// The contested key.
        key: String,
    },
    /// A type name is not a legal identifier in all three target languages.
    IllegalName {
        /// The offending name.
        name: String,
    },
    /// A field key is not a legal path segment.
    ///
    /// A key containing `.`, `[` or `]` renders into a path string that parses
    /// back as a *different* path — one segment in, two out — which is exactly
    /// the hazard [`Path::from_segments`](duet_core::Path::from_segments)
    /// documents and only a `debug_assert!` catches.
    IllegalKey {
        /// The type holding the field.
        type_name: String,
        /// The offending key.
        key: String,
    },
    /// Two commands claimed the same name.
    ///
    /// A guest names a command and nothing else, so one `invoke` would mean
    /// two things and a registry would silently keep whichever was inserted
    /// last.
    CommandCollision {
        /// The contested name.
        name: String,
    },
    /// A command name is not a dotted sequence of legal identifiers.
    IllegalCommandName {
        /// The offending name.
        name: String,
    },
    /// A command parameter's key is not a legal wire key.
    ///
    /// The same rule a struct field's key follows: a parameter occupies a key
    /// in the `args` map exactly as a field occupies one in a `Value::Map`, and
    /// the schema has one notion of a wire key rather than two.
    IllegalParam {
        /// The command holding the parameter.
        command: String,
        /// The offending key.
        key: String,
    },
    /// Two parameters of one command map onto the same key.
    ///
    /// They would occupy one entry of the `args` map: one of the two can never
    /// be supplied, and which one is not something the developer chose.
    DuplicateParam {
        /// The command holding both parameters.
        command: String,
        /// The contested key.
        key: String,
    },
    /// A [`Ty::Named`](crate::Ty::Named) refers to a type nothing defined.
    UnknownType {
        /// The dangling name.
        name: String,
    },
    /// The schema describes a value nested deeper than the store accepts.
    ///
    /// Every write of the root would be refused by
    /// [`Store::set`](duet_core::Store::set), so this is a schema that can
    /// never be installed. Caught here rather than on the first write.
    TooDeep {
        /// The nesting the schema's own structure implies.
        depth: usize,
        /// The most it may have — [`MAX_VALUE_DEPTH`](duet_core::MAX_VALUE_DEPTH).
        max: usize,
    },
}

impl std::fmt::Display for SchemaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SchemaError::Recursive { chain } => {
                write!(f, "recursive type: {}", chain.join(" -> "))
            }
            SchemaError::NameCollision { name } => write!(
                f,
                "two different types are both named \"{}\"",
                echo::truncate(name)
            ),
            SchemaError::DuplicateKey { type_name, key } => write!(
                f,
                "type \"{}\" has two fields on the key \"{}\"",
                echo::truncate(type_name),
                echo::truncate(key)
            ),
            SchemaError::IllegalName { name } => write!(
                f,
                "\"{}\" is not a legal type name: expected an ASCII letter \
                 followed by letters, digits or inner underscores",
                echo::truncate(name)
            ),
            SchemaError::IllegalKey { type_name, key } => write!(
                f,
                "key \"{}\" on type \"{}\" is not a legal path segment: \
                 a key may not be empty or contain '.', '[' or ']'",
                echo::truncate(key),
                echo::truncate(type_name)
            ),
            SchemaError::CommandCollision { name } => write!(
                f,
                "two different commands are both named \"{}\"",
                echo::truncate(name)
            ),
            SchemaError::IllegalCommandName { name } => write!(
                f,
                "\"{}\" is not a legal command name: expected identifiers \
                 separated by '.', each an ASCII letter followed by letters, \
                 digits or inner underscores",
                echo::truncate(name)
            ),
            SchemaError::IllegalParam { command, key } => write!(
                f,
                "parameter \"{}\" of command \"{}\" is not a legal wire key: \
                 a key may not be empty or contain '.', '[' or ']'",
                echo::truncate(key),
                echo::truncate(command)
            ),
            SchemaError::DuplicateParam { command, key } => write!(
                f,
                "command \"{}\" has two parameters on the key \"{}\"",
                echo::truncate(command),
                echo::truncate(key)
            ),
            SchemaError::UnknownType { name } => {
                write!(f, "no type named \"{}\" is defined", echo::truncate(name))
            }
            SchemaError::TooDeep { depth, max } => write!(
                f,
                "the schema nests {depth} containers, past the store's limit of {max}"
            ),
        }
    }
}

impl std::error::Error for SchemaError {}

/// Every reason a schema could not be built, in the order they were found.
///
/// A `Vec` rather than a single error because the first problem is rarely the
/// only one, and a developer fixing type definitions benefits from seeing all
/// of them at once rather than recompiling per error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaErrors(pub(crate) Vec<SchemaError>);

impl SchemaErrors {
    /// The individual problems.
    pub fn as_slice(&self) -> &[SchemaError] {
        &self.0
    }

    /// Consumes this and yields the problems.
    #[must_use]
    pub fn into_vec(self) -> Vec<SchemaError> {
        self.0
    }
}

impl std::fmt::Display for SchemaErrors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut first = true;
        for error in &self.0 {
            if !first {
                write!(f, "; ")?;
            }
            write!(f, "{error}")?;
            first = false;
        }
        if first {
            // Unreachable through `Schema::of`, which only builds this when it
            // has at least one error — but `Display` must still be total, and
            // an empty rendering would be a silently blank panic message.
            write!(f, "no schema errors")?;
        }
        Ok(())
    }
}

impl std::error::Error for SchemaErrors {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_variant_renders_informatively() {
        let cases: [(SchemaError, &str); 11] = [
            (
                SchemaError::Recursive {
                    chain: vec!["Node".into(), "Node".into()],
                },
                "recursive type: Node -> Node",
            ),
            (
                SchemaError::NameCollision {
                    name: "Config".into(),
                },
                "two different types are both named \"Config\"",
            ),
            (
                SchemaError::DuplicateKey {
                    type_name: "Editor".into(),
                    key: "zoom".into(),
                },
                "type \"Editor\" has two fields on the key \"zoom\"",
            ),
            (
                SchemaError::IllegalName {
                    name: "2Bad".into(),
                },
                "\"2Bad\" is not a legal type name: expected an ASCII letter \
                 followed by letters, digits or inner underscores",
            ),
            (
                SchemaError::IllegalKey {
                    type_name: "Editor".into(),
                    key: "a.b".into(),
                },
                "key \"a.b\" on type \"Editor\" is not a legal path segment: \
                 a key may not be empty or contain '.', '[' or ']'",
            ),
            (
                SchemaError::CommandCollision {
                    name: "bump".into(),
                },
                "two different commands are both named \"bump\"",
            ),
            (
                SchemaError::IllegalCommandName {
                    name: "2fast".into(),
                },
                "\"2fast\" is not a legal command name: expected identifiers \
                 separated by '.', each an ASCII letter followed by letters, \
                 digits or inner underscores",
            ),
            (
                SchemaError::IllegalParam {
                    command: "bump".into(),
                    key: "a.b".into(),
                },
                "parameter \"a.b\" of command \"bump\" is not a legal wire key: \
                 a key may not be empty or contain '.', '[' or ']'",
            ),
            (
                SchemaError::DuplicateParam {
                    command: "bump".into(),
                    key: "by".into(),
                },
                "command \"bump\" has two parameters on the key \"by\"",
            ),
            (
                SchemaError::UnknownType {
                    name: "Ghost".into(),
                },
                "no type named \"Ghost\" is defined",
            ),
            (
                SchemaError::TooDeep { depth: 62, max: 61 },
                "the schema nests 62 containers, past the store's limit of 61",
            ),
        ];
        for (error, expected) in cases {
            assert_eq!(error.to_string(), expected);
        }
    }

    #[test]
    fn a_long_type_name_cannot_grow_a_message() {
        let rendered = SchemaError::NameCollision {
            name: "N".repeat(100_000),
        }
        .to_string();
        assert!(rendered.len() < 256, "got {} bytes", rendered.len());
    }

    #[test]
    fn errors_render_joined() {
        let errors = SchemaErrors(vec![
            SchemaError::UnknownType {
                name: "Ghost".into(),
            },
            SchemaError::TooDeep { depth: 62, max: 61 },
        ]);
        assert_eq!(
            errors.to_string(),
            "no type named \"Ghost\" is defined; \
             the schema nests 62 containers, past the store's limit of 61"
        );
        assert_eq!(errors.as_slice().len(), 2);
        assert_eq!(errors.into_vec().len(), 2);
    }

    #[test]
    fn an_empty_error_list_still_renders_something() {
        assert_eq!(SchemaErrors(Vec::new()).to_string(), "no schema errors");
    }

    #[test]
    fn schema_errors_implement_std_error() {
        let one: &dyn std::error::Error = &SchemaError::TooDeep { depth: 62, max: 61 };
        let many: &dyn std::error::Error =
            &SchemaErrors(vec![SchemaError::TooDeep { depth: 62, max: 61 }]);
        assert!(one.to_string().contains("62"));
        assert!(many.to_string().contains("62"));
    }
}
