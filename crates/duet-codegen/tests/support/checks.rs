//! The checks that read a **generated file** and judge it, written so they can
//! be pointed at something other than the real goldens.
//!
//! `tests/real_host.rs` runs them on the committed artifacts. `tests/mutation.rs`
//! runs them on deliberately corrupted copies and asserts which of them notices
//! — because a check nobody has ever seen fail is a check nobody knows works.
//!
//! Everything here takes its input from generated **text**, never from a `Plan`.
//! A check that read the emitter's own model would agree with the emitter by
//! construction, which is the exact failure mode a golden test already has.

#![allow(dead_code)]

use duet_core::{Path, Segment, Store, Value};
use duet_schema::{Schema, Ty, TypeDef};

/// Which language a generated file is in; the codec spellings differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    Dart,
    TypeScript,
}

/// One `(path, codec)` binding found in a generated file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub path: String,
    pub codec: String,
}

/// Every field binding in `text`.
///
/// Both emitters spell one as `…router, '<path>', <codec>)`, so one scanner
/// serves both. Deliberately dumb: a smarter parser could share a bug with the
/// emitter it is checking.
pub fn bindings(text: &str) -> Vec<Binding> {
    let mut found = Vec::new();
    for after in text.split("router, '").skip(1) {
        let Some(end) = after.find('\'') else {
            continue;
        };
        let rest = after[end + 1..].trim_start_matches([',', ' ', '\n']);
        found.push(Binding {
            path: after[..end].to_string(),
            codec: until_unmatched_paren(rest),
        });
    }
    found
}

/// `text` up to the first `)` that closes something it did not open.
fn until_unmatched_paren(text: &str) -> String {
    let mut depth = 0usize;
    for (at, ch) in text.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' if depth == 0 => return text[..at].to_string(),
            ')' => depth -= 1,
            _ => {}
        }
    }
    text.to_string()
}

/// Resolves every path in `text` against a real store seeded from `schema`.
///
/// # Errors
///
/// A description of the first path that addresses nothing, holds the wrong kind
/// of node, or cannot be written to.
pub fn paths_resolve(text: &str, schema: &Schema) -> Result<usize, String> {
    let mut store = Store::new(seed(schema.root(), schema.types()));
    let found = bindings(text);
    for binding in &found {
        let path = Path::parse(&binding.path)
            .map_err(|e| format!("{:?} is not a legal path: {e}", binding.path))?;
        let expected = resolve(schema, &path)
            .ok_or_else(|| format!("{:?} is not a path the schema has", binding.path))?;
        let held = store
            .get(&path)
            .ok_or_else(|| format!("{:?} addresses no node on a real store", binding.path))?;
        if !fits(&expected, held) {
            return Err(format!(
                "{:?} holds {held:?} but the schema says {expected:?}",
                binding.path
            ));
        }
        store
            .set(&path, seed(&expected, schema.types()))
            .map_err(|e| format!("writing {:?} was refused: {e}", binding.path))?;
    }
    Ok(found.len())
}

/// Checks every path segment is a key some type in `schema` declares, verbatim.
///
/// Complements [`paths_resolve`], which would still pass if the schema and the
/// emitter had been camel-cased *together*.
///
/// # Errors
///
/// A description of the first segment no type declares.
pub fn segments_are_schema_keys(text: &str, schema: &Schema) -> Result<usize, String> {
    let keys: Vec<&str> = schema
        .types()
        .iter()
        .flat_map(|t| t.fields.iter().map(|f| f.key.as_str()))
        .collect();
    let found = bindings(text);
    for binding in &found {
        for segment in binding.path.split('.').filter(|s| !s.is_empty()) {
            if !keys.contains(&segment) {
                return Err(format!(
                    "{:?} has a segment {segment:?} no schema type declares",
                    binding.path
                ));
            }
        }
    }
    Ok(found.len())
}

/// Checks the codec bound at each path is the one that path's schema type calls
/// for.
///
/// # This is a restatement, and that is worth saying
///
/// The table below is a second, separately written copy of the emitters'
/// mapping. It catches a change made on one side and not the other — which is
/// what mutation testing measures — but it is **not** independent evidence that
/// the mapping is right. That evidence is in the guest packages: a test there
/// reads an `int` path through the generated client against a host serving a
/// `Value::Int`, and a float codec reports a mismatch rather than a value.
///
/// # Errors
///
/// A description of the first path whose codec disagrees.
pub fn codecs_match_the_schema(
    text: &str,
    schema: &Schema,
    language: Language,
) -> Result<usize, String> {
    let found = bindings(text);
    for binding in &found {
        let path = Path::parse(&binding.path)
            .map_err(|e| format!("{:?} is not a legal path: {e}", binding.path))?;
        let expected = resolve(schema, &path)
            .ok_or_else(|| format!("{:?} is not a path the schema has", binding.path))?;
        let want = codec_for(&unwrap_optional(&expected), language);
        if binding.codec != want {
            return Err(format!(
                "{:?} binds {} but its schema type calls for {want}",
                binding.path, binding.codec
            ));
        }
    }
    Ok(found.len())
}

/// The codec expression a `Ty` calls for, restated independently of the
/// emitters.
fn codec_for(ty: &Ty, language: Language) -> String {
    let dart = language == Language::Dart;
    match ty {
        Ty::Bool => "duetBoolCodec".to_string(),
        Ty::Int => "duetIntCodec".to_string(),
        Ty::Float => "duetFloatCodec".to_string(),
        Ty::Str => "duetStringCodec".to_string(),
        Ty::Bytes => "duetBytesCodec".to_string(),
        Ty::Dynamic => "duetDynamicCodec".to_string(),
        Ty::List(item) => format!(
            "duetListCodec<{}>({})",
            type_for(item, language),
            codec_for(item, language)
        ),
        Ty::Map(value) => format!(
            "duetMapCodec<{}>({})",
            type_for(value, language),
            codec_for(value, language)
        ),
        Ty::Named(name) if dart => format!("const {name}Codec()"),
        Ty::Named(name) => {
            let mut chars = name.chars();
            match chars.next() {
                Some(first) => format!("{}{}Codec", first.to_ascii_lowercase(), chars.as_str()),
                None => "Codec".to_string(),
            }
        }
        other => format!("<no codec for {other:?}>"),
    }
}

/// The guest type a `Ty` calls for, restated independently of the emitters.
fn type_for(ty: &Ty, language: Language) -> String {
    let dart = language == Language::Dart;
    match ty {
        Ty::Bool if dart => "bool".to_string(),
        Ty::Bool => "boolean".to_string(),
        Ty::Int if dart => "int".to_string(),
        Ty::Int => "bigint".to_string(),
        Ty::Float if dart => "double".to_string(),
        Ty::Float => "number".to_string(),
        Ty::Str if dart => "String".to_string(),
        Ty::Str => "string".to_string(),
        Ty::Bytes if dart => "List<int>".to_string(),
        Ty::Bytes => "Uint8Array".to_string(),
        Ty::Dynamic => "DuetValue".to_string(),
        Ty::List(item) if dart => format!("List<{}>", type_for(item, language)),
        Ty::List(item) => format!("{}[]", type_for(item, language)),
        Ty::Map(value) if dart => format!("Map<String, {}>", type_for(value, language)),
        Ty::Map(value) => format!("Map<string, {}>", type_for(value, language)),
        Ty::Named(name) => name.clone(),
        other => format!("<no type for {other:?}>"),
    }
}

/// `Option<T>` down to `T`; anything else unchanged.
fn unwrap_optional(ty: &Ty) -> Ty {
    match ty {
        Ty::Optional(inner) => inner.as_ref().clone(),
        other => other.clone(),
    }
}

/// A value matching `ty`, with every optional **present** so every path below it
/// exists.
///
/// An `Option<Editor>` set to `None` makes `editor.zoom` genuinely absent on the
/// host — measured, documented behaviour, and what `DuetOptionalField` exists
/// for. Seeding it present is what lets a check reach every literal rather than
/// only the ones that happen to be reachable.
pub fn seed(ty: &Ty, types: &[TypeDef]) -> Value {
    match ty {
        Ty::Bool => Value::Bool(false),
        Ty::Int => Value::Int(0),
        Ty::Float => Value::Float(0.0),
        Ty::Str => Value::Str(String::new()),
        Ty::Bytes => Value::Bytes(Vec::new()),
        Ty::Dynamic => Value::Null,
        Ty::Optional(inner) => seed(inner, types),
        Ty::List(_) => Value::List(Vec::new()),
        Ty::Map(_) => Value::map([]),
        Ty::Named(name) => match types.iter().find(|t| &t.name == name) {
            Some(def) => Value::map(
                def.fields
                    .iter()
                    .map(|f| (f.key.as_str(), seed(&f.ty, types)))
                    .collect::<Vec<_>>(),
            ),
            None => Value::Null,
        },
        _ => Value::Null,
    }
}

/// What the schema says lives at `path`, or `None` if it says nothing.
pub fn resolve(schema: &Schema, path: &Path) -> Option<Ty> {
    let mut ty = schema.root().clone();
    for segment in path.segments() {
        let Segment::Key(key) = segment else {
            return None;
        };
        let name = match unwrap_optional(&ty) {
            Ty::Named(name) => name,
            _ => return None,
        };
        let def = schema.types().iter().find(|t| t.name == name)?;
        ty = def.fields.iter().find(|f| &f.key == key)?.ty.clone();
    }
    Some(ty)
}

/// True if `value` is the kind of node `ty` describes.
///
/// `Ty::Dynamic` fits everything, which is the arm's definition rather than a
/// weakening: the schema says nothing about a dynamic field, so nothing there
/// can contradict it.
pub fn fits(ty: &Ty, value: &Value) -> bool {
    match ty {
        Ty::Bool => matches!(value, Value::Bool(_)),
        Ty::Int => matches!(value, Value::Int(_)),
        Ty::Float => matches!(value, Value::Float(_)),
        Ty::Str => matches!(value, Value::Str(_)),
        Ty::Bytes => matches!(value, Value::Bytes(_)),
        Ty::Dynamic => true,
        Ty::Optional(inner) => matches!(value, Value::Null) || fits(inner, value),
        Ty::List(_) => matches!(value, Value::List(_)),
        Ty::Map(_) | Ty::Named(_) => matches!(value, Value::Map(_)),
        _ => false,
    }
}
