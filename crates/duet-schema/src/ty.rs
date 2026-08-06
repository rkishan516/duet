//! The schema's type language: [`Ty`], [`TypeDef`] and [`FieldDef`].

/// A type as the schema describes it.
///
/// This is a *lowered* vocabulary, not a mirror of Rust's type system. Several
/// Rust types collapse onto one arm — `i8`, `u32` and `i64` are all [`Ty::Int`]
/// — because the wire has exactly one integer type and the guests have to agree
/// with the host about what a path holds. The narrowing lives in the Rust
/// decode, where a value that does not fit is a reportable mismatch; it is not
/// a promise the schema can make to Dart or TypeScript, neither of which has a
/// `u8`.
///
/// `#[non_exhaustive]` because Phase 4b adds arms (`char`, fixed-size arrays,
/// tuples, data enums). A `match` in downstream code needs a `_` arm today so
/// that adding one later is not a breaking change for it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Ty {
    /// `bool` — [`Value::Bool`](duet_core::Value::Bool).
    Bool,
    /// Any Rust integer that fits `i64` — [`Value::Int`](duet_core::Value::Int).
    Int,
    /// `f64` — [`Value::Float`](duet_core::Value::Float).
    Float,
    /// `String` — [`Value::Str`](duet_core::Value::Str).
    Str,
    /// [`Bytes`](crate::Bytes) — [`Value::Bytes`](duet_core::Value::Bytes).
    Bytes,
    /// [`duet_core::Value`] itself: whatever a guest put there.
    ///
    /// The escape hatch that keeps a schema from having to describe data the
    /// application treats as opaque. A `dynamic` field is the one place a
    /// generated typed accessor hands back a raw `Value`.
    Dynamic,
    /// `Option<T>` — the inner `Ty`, or [`Value::Null`](duet_core::Value::Null).
    Optional(Box<Ty>),
    /// `Vec<T>` — [`Value::List`](duet_core::Value::List) of the inner `Ty`.
    List(Box<Ty>),
    /// A string-keyed map — [`Value::Map`](duet_core::Value::Map) whose every
    /// value is the inner `Ty`.
    ///
    /// Only the *value* type is carried: the key type is `String` by
    /// construction, because [`Value::Map`](duet_core::Value::Map) has no other
    /// kind of key and path addressing depends on that.
    Map(Box<Ty>),
    /// A reference to a [`TypeDef`] in the [`Registry`](crate::Registry).
    ///
    /// Carries the name rather than the definition so that a type used in ten
    /// places is described once. [`Schema::build`](crate::Schema) checks every
    /// name here resolves.
    Named(String),
}

impl Ty {
    /// Wraps `self` in [`Ty::Optional`].
    #[must_use]
    pub fn optional(self) -> Ty {
        Ty::Optional(Box::new(self))
    }

    /// Wraps `self` in [`Ty::List`].
    #[must_use]
    pub fn list(self) -> Ty {
        Ty::List(Box::new(self))
    }

    /// Wraps `self` in [`Ty::Map`] as the map's *value* type.
    #[must_use]
    pub fn map(self) -> Ty {
        Ty::Map(Box::new(self))
    }

    /// The names of every [`Ty::Named`] reachable from here, in the order they
    /// appear.
    ///
    /// Iterative, not recursive: the walk that finds a pathological schema must
    /// not itself be pathological, and a Rust stack overflow aborts rather than
    /// erroring. The explicit stack holds at most one entry per node in a `Ty`
    /// the caller already constructed.
    pub(crate) fn referenced_names(&self) -> Vec<&str> {
        let mut found = Vec::new();
        let mut pending = vec![self];
        while let Some(node) = pending.pop() {
            match node {
                Ty::Named(name) => found.push(name.as_str()),
                Ty::Optional(inner) | Ty::List(inner) | Ty::Map(inner) => pending.push(inner),
                _ => {}
            }
        }
        found
    }
}

/// One field of a struct, as the schema describes it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FieldDef {
    /// The **wire key** — the map key this field occupies, and therefore one
    /// segment of every path that addresses it.
    ///
    /// Checked against [`Path::parse`](duet_core::Path)'s grammar when the
    /// schema is built, because a key containing `.`, `[` or `]` would render
    /// into a path string that parses back as a *different* path.
    pub key: String,
    /// What the field holds.
    pub ty: Ty,
}

impl FieldDef {
    /// Describes a field.
    pub fn new(key: impl Into<String>, ty: Ty) -> FieldDef {
        FieldDef {
            key: key.into(),
            ty,
        }
    }
}

/// A named struct in the schema.
///
/// Field order is **declaration order**, not sorted: it is what generated
/// constructors in Dart and TypeScript use for positional arguments, so
/// reordering it is a source-breaking change for a generated client and must
/// be a deliberate edit to the Rust type rather than an emergent property of a
/// map's iteration order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeDef {
    /// The type's name, unique across the schema.
    pub name: String,
    /// Its fields, in declaration order.
    pub fields: Vec<FieldDef>,
}

/// True if `name` is a legal schema type name.
///
/// Deliberately narrower than Rust's own identifier rules — ASCII only, leading
/// letter, then letters and digits — because the name has to be a legal
/// identifier in **Dart and TypeScript too**, and in both of those a name is
/// also a filename. Underscores are allowed inside so that a Rust type named
/// `App_State` survives, but a name may not start or end with one: those are
/// the spellings the emitters use for their own private symbols.
pub(crate) fn is_legal_type_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_ascii_alphabetic()
        && !name.ends_with('_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// True if `key` is a legal wire key: non-empty, and free of the three
/// characters [`Path::parse`](duet_core::Path) gives meaning to.
///
/// The real check is the round-trip in [`Schema`](crate::Schema), which runs
/// every key through the actual parser. This predicate exists so the common
/// rejection reports *which* character was the problem rather than a parse
/// offset.
pub(crate) fn is_legal_key(key: &str) -> bool {
    !key.is_empty() && !key.contains(['.', '[', ']'])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wrappers_nest_as_written() {
        assert_eq!(Ty::Int.list(), Ty::List(Box::new(Ty::Int)));
        assert_eq!(Ty::Str.optional(), Ty::Optional(Box::new(Ty::Str)));
        assert_eq!(Ty::Bool.map(), Ty::Map(Box::new(Ty::Bool)));
        assert_eq!(
            Ty::Int.list().optional(),
            Ty::Optional(Box::new(Ty::List(Box::new(Ty::Int))))
        );
    }

    #[test]
    fn referenced_names_finds_every_nesting_position() {
        assert_eq!(Ty::Int.referenced_names(), Vec::<&str>::new());
        assert_eq!(Ty::Named("Editor".into()).referenced_names(), ["Editor"]);
        assert_eq!(
            Ty::Named("Editor".into())
                .list()
                .optional()
                .referenced_names(),
            ["Editor"]
        );
        assert_eq!(Ty::Named("Doc".into()).map().referenced_names(), ["Doc"]);
    }

    #[test]
    fn legal_type_names_are_ascii_identifiers() {
        for good in ["A", "AppState", "App_State", "Editor2", "lowerCamel"] {
            assert!(is_legal_type_name(good), "{good} should be legal");
        }
    }

    #[test]
    fn illegal_type_names_are_rejected() {
        for bad in [
            "",
            "_Leading",
            "Trailing_",
            "2Digits",
            "Has Space",
            "Has-Dash",
            "Café",
            "App.State",
        ] {
            assert!(!is_legal_type_name(bad), "{bad} should be rejected");
        }
    }

    #[test]
    fn legal_keys_avoid_the_path_metacharacters() {
        for good in ["a", "zoom", "snake_case", "café", "with space", "3"] {
            assert!(is_legal_key(good), "{good} should be legal");
        }
        for bad in ["", "a.b", "a[0]", "a]b"] {
            assert!(!is_legal_key(bad), "{bad} should be rejected");
        }
    }
}
