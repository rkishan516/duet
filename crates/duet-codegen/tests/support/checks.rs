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

/// One command method found in a generated file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandBinding {
    /// The wire name the method invokes, exactly as the literal spells it.
    pub name: String,
    /// The argument keys it sends, in the order they are written.
    pub keys: Vec<String>,
    /// The codec bound to the `returned` arm.
    pub returns: String,
    /// The codec bound to the `raised` arm.
    pub raises: String,
}

/// Every command method in `text`.
///
/// Both emitters open an invocation with `invoke('<name>'`, put one argument per
/// line beginning with its quoted key, and then write the return codec and the
/// error codec on the two lines that follow. One scanner therefore serves both.
/// Deliberately dumb, for the reason [`bindings`] is: a smarter parser could
/// share a bug with the emitter it is checking.
pub fn command_bindings(text: &str) -> Vec<CommandBinding> {
    let mut found = Vec::new();
    for after in text.split("invoke('").skip(1) {
        let Some(end) = after.find('\'') else {
            continue;
        };
        let name = after[..end].to_string();
        let rest = &after[end + 1..];
        let mut keys = Vec::new();
        let mut codecs = Vec::new();
        for line in rest.lines().skip(1) {
            let trimmed = line.trim();
            // An argument line opens with its quoted key; a bracket line closes
            // the argument literal; anything else is the first codec.
            if let Some(key) = argument_key(trimmed) {
                keys.push(key);
                continue;
            }
            if trimmed.starts_with('}') || trimmed.starts_with(']') {
                continue;
            }
            codecs.push(trimmed.trim_end_matches(',').to_string());
            if codecs.len() == 2 {
                break;
            }
        }
        found.push(CommandBinding {
            name,
            keys,
            returns: codecs.first().cloned().unwrap_or_default(),
            raises: codecs.get(1).cloned().unwrap_or_default(),
        });
    }
    found
}

/// The argument key a generated line opens with, if it opens with one.
///
/// `'by': …` in Dart and `['by', …]` in TypeScript.
fn argument_key(trimmed: &str) -> Option<String> {
    let opened = trimmed.strip_prefix("['").or_else(|| {
        trimmed
            .strip_prefix('\'')
            .filter(|_| trimmed.contains("':"))
    })?;
    let end = opened.find('\'')?;
    Some(opened[..end].to_string())
}

/// Checks every command a generated file invokes is one `schema` declares, with
/// exactly the parameter keys it declares.
///
/// The command-side counterpart of [`segments_are_schema_keys`], and the check a
/// golden comparison cannot make: a camel-cased command name is not a syntax
/// error, not a type error and not a decode error — it is a refusal at run time.
/// `crates/duet-host-stdio/tests/commands.rs` takes the same scan one step
/// further and resolves each name against a **live registry**.
///
/// # Errors
///
/// A description of the first command the schema does not declare, or the first
/// argument list that disagrees with the declared parameters.
pub fn commands_resolve(text: &str, schema: &Schema) -> Result<usize, String> {
    let found = command_bindings(text);
    for binding in &found {
        let declared = schema
            .commands()
            .iter()
            .find(|c| c.name == binding.name)
            .ok_or_else(|| format!("{:?} is not a command the schema has", binding.name))?;
        let mut expected: Vec<&str> = declared.params.iter().map(|p| p.key.as_str()).collect();
        let mut sent: Vec<&str> = binding.keys.iter().map(String::as_str).collect();
        expected.sort_unstable();
        sent.sort_unstable();
        if sent != expected {
            return Err(format!(
                "{:?} is called with {sent:?} but the schema declares {expected:?}",
                binding.name
            ));
        }
    }
    Ok(found.len())
}

/// Checks the codecs bound to each command's two reply arms are the ones its
/// schema types call for.
///
/// A restatement of the emitters' mapping, exactly as
/// [`codecs_match_the_schema`] is, and worth the same caveat: it catches a
/// change made on one side and not the other. The independent evidence that the
/// mapping is right is in the guest packages, where a generated method is driven
/// against a live host and the decoded value is compared to an exact literal.
///
/// # Errors
///
/// A description of the first arm whose codec disagrees.
pub fn command_codecs_match_the_schema(
    text: &str,
    schema: &Schema,
    language: Language,
) -> Result<usize, String> {
    let found = command_bindings(text);
    for binding in &found {
        let declared = schema
            .commands()
            .iter()
            .find(|c| c.name == binding.name)
            .ok_or_else(|| format!("{:?} is not a command the schema has", binding.name))?;
        for (what, ty, bound) in [
            ("returns", declared.returns.as_ref(), &binding.returns),
            ("raises", declared.raises.as_ref(), &binding.raises),
        ] {
            // An undeclared type is generated with the identity codec, because
            // the wire still answers there — with null.
            let want = codec_for(ty.unwrap_or(&Ty::Dynamic), language);
            if bound != &want {
                return Err(format!(
                    "{:?} binds {bound} for {what} but its schema type calls for {want}",
                    binding.name
                ));
            }
        }
    }
    Ok(found.len())
}
