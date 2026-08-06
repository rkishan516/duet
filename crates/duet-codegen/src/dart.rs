//! The Dart emitter.
//!
//! Produces declarations, literals and delegation — nothing else. A reviewer
//! should be able to check a whole generated diff by reading it, which is only
//! true while every algorithm stays in `package:duet/typed.dart`.

use duet_schema::Ty;

use crate::emit::{Options, header};
use crate::plan::{Plan, PlannedAccessor, PlannedClass, PlannedField, PlannedTy, PlannedType};

/// Emits the whole Dart client.
pub(crate) fn emit(plan: &Plan, options: &Options) -> String {
    let mut out = header(options, "Dart");
    out.push_str(&format!(
        "\n/// The typed Duet client generated from `{}`.\nlibrary;\n\n\
         import 'package:duet/duet.dart';\nimport 'package:duet/typed.dart';\n",
        options.source
    ));
    for planned in &plan.types {
        out.push('\n');
        emit_data_class(&mut out, planned);
        out.push('\n');
        emit_codec(&mut out, planned);
    }
    for class in &plan.classes {
        out.push('\n');
        emit_client(&mut out, class);
    }
    out
}

/// `final class Editor { … }` — fields, value equality and a `toString`.
fn emit_data_class(out: &mut String, planned: &PlannedType) {
    let name = &planned.name;
    out.push_str(&format!("/// `{name}`, as the schema describes it.\n"));
    out.push_str(&format!("final class {name} {{\n"));
    out.push_str(&format!("  /// Creates {}.\n", an(name)));
    if planned.fields.is_empty() {
        out.push_str(&format!("  const {name}();\n"));
    } else {
        let params: Vec<String> = planned
            .fields
            .iter()
            .map(|f| format!("required this.{}", f.accessor))
            .collect();
        out.push_str(&format!(
            "  const {name}({});\n",
            listed("{", &params, "}", "  ")
        ));
    }
    for field in &planned.fields {
        out.push_str(&format!(
            "\n  /// The `{}` field.\n  final {} {};\n",
            field.key,
            dart_ty(&field.ty),
            field.accessor
        ));
    }
    emit_equality(out, planned);
    out.push_str("}\n");
}

/// `==`, `hashCode` and `toString`, in terms of every field.
fn emit_equality(out: &mut String, planned: &PlannedType) {
    let name = &planned.name;
    let comparisons = planned
        .fields
        .iter()
        .map(|f| format!(" &&\n      other.{0} == {0}", f.accessor))
        .collect::<String>();
    out.push_str(&format!(
        "\n  @override\n  bool operator ==(Object other) =>\n      other is {name}{comparisons};\n"
    ));
    // `hashAll` rather than `Object.hash`, which takes at most twenty
    // arguments: a schema type with twenty-one fields must not be the thing
    // that stops generating.
    let hashed: Vec<String> = planned.fields.iter().map(|f| f.accessor.clone()).collect();
    out.push_str(&format!(
        "\n  @override\n  int get hashCode => Object.hashAll(<Object?>{});\n",
        listed("[", &hashed, "]", "  ")
    ));
    let described = planned
        .fields
        .iter()
        .map(|f| format!("{0}: ${0}", f.accessor))
        .collect::<Vec<_>>()
        .join(", ");
    out.push_str(&format!(
        "\n  @override\n  String toString() =>\n      '{name}({described})';\n"
    ));
}

/// `final class EditorCodec implements DuetCodec<Editor> { … }`.
fn emit_codec(out: &mut String, planned: &PlannedType) {
    let name = &planned.name;
    out.push_str(&format!(
        "/// The [DuetCodec] for [{name}].\nfinal class {name}Codec implements DuetCodec<{name}> {{\n\
         \x20 /// Creates the codec.\n  const {name}Codec();\n\n\
         \x20 @override\n  String get name => '{name}';\n\n"
    ));
    emit_encode(out, planned);
    out.push('\n');
    emit_decode(out, planned);
    out.push_str("}\n");
}

/// The encoder: one map entry per field, each delegating to a codec.
fn emit_encode(out: &mut String, planned: &PlannedType) {
    let name = &planned.name;
    out.push_str(&format!(
        "  @override\n  DuetValue encode({name} value) {{\n"
    ));
    // Locals so Dart's flow analysis can promote a nullable field to
    // non-nullable inside the conditional; a field access cannot be promoted,
    // and the alternative is a `!` in generated code.
    for field in planned.fields.iter().filter(|f| f.ty.optional) {
        out.push_str(&format!(
            "    final {} {} = value.{};\n",
            dart_ty(&field.ty),
            field.accessor,
            field.accessor
        ));
    }
    out.push_str("    return DuetMap(<String, DuetValue>{\n");
    for field in &planned.fields {
        let codec = dart_codec(&field.ty.inner);
        let expression = if field.ty.optional {
            format!(
                "{0} == null ? const DuetNull() : {codec}.encode({0})",
                field.accessor
            )
        } else {
            format!("{codec}.encode(value.{})", field.accessor)
        };
        out.push_str(&format!("      '{}': {expression},\n", field.key));
    }
    out.push_str("    });\n  }\n");
}

/// The decoder: one reading per field, then the constructor call.
///
/// Every field goes through `duetRequiredReading` or `duetOptionalReading` —
/// the same two hand-written, hand-tested functions a field's `get` uses — so
/// the absent / null / mismatch distinctions inside a struct are exactly the
/// ones at the top level, rather than a second implementation of them.
fn emit_decode(out: &mut String, planned: &PlannedType) {
    let name = &planned.name;
    out.push_str(&format!(
        "  @override\n  {name}? decode(DuetValue value) {{\n    if (value is! DuetMap) return null;\n"
    ));
    for field in &planned.fields {
        let inner = dart_inner_ty(&field.ty.inner);
        let reader = if field.ty.optional {
            "duetOptionalReading"
        } else {
            "duetRequiredReading"
        };
        out.push_str(&format!(
            "    final DuetReading<{inner}> {0} =\n        {reader}<{inner}>({1}, value.entries['{2}']);\n",
            field.accessor,
            dart_codec(&field.ty.inner),
            field.key
        ));
        if field.ty.optional {
            out.push_str(&format!(
                "    if ({0} is DuetAbsent<{inner}> || {0} is DuetMismatch<{inner}>) return null;\n",
                field.accessor
            ));
        } else {
            out.push_str(&format!(
                "    if ({0} is! DuetPresent<{inner}>) return null;\n",
                field.accessor
            ));
        }
    }
    let arguments: Vec<String> = planned.fields.iter().map(dart_argument).collect();
    out.push_str(&format!(
        "    return {name}{};\n  }}\n",
        listed("(", &arguments, ")", "    ")
    ));
}

/// The most items that stay on one line.
///
/// A fixed count rather than a measured line width, so the wrapping is a
/// function of the schema alone: a golden must not change because a field was
/// renamed by one character.
const INLINE_LIMIT: usize = 3;

/// `open`, `items`, `close` — on one line while there are few enough of them,
/// one per line and trailing-comma'd once there are not.
fn listed(open: &str, items: &[String], close: &str, indent: &str) -> String {
    if items.len() <= INLINE_LIMIT {
        return format!("{open}{}{close}", items.join(", "));
    }
    let body = items
        .iter()
        .map(|item| format!("{indent}  {item},\n"))
        .collect::<String>();
    format!("{open}\n{body}{indent}{close}")
}

/// How one decoded reading is handed to the constructor.
fn dart_argument(field: &PlannedField) -> String {
    if field.ty.optional {
        // `valueOrNull` is null for both `DuetNone` and the arms already
        // rejected above, so by here it means exactly `Option::None`.
        format!("{0}: {0}.valueOrNull", field.accessor)
    } else {
        format!("{0}: {0}.value", field.accessor)
    }
}

/// One accessor class: the router, the struct's own handle, and one accessor
/// per field.
fn emit_client(out: &mut String, class: &PlannedClass) {
    let PlannedClass {
        name,
        type_name,
        path,
        optional,
        accessors,
    } = class;
    // An `Option<Editor>` reached through this class must answer *none* rather
    // than a mismatch, so the struct's own handle follows the field that
    // reached it.
    let handle = if *optional {
        "DuetOptionalField"
    } else {
        "DuetField"
    };
    out.push_str(&format!(
        "/// Typed accessors for `{type_name}` at {}.\n\
         ///\n\
         /// Every path here is a literal; see this file's header.\n\
         final class {name} {{\n\
         \x20 /// Binds these accessors to [router].\n  const {name}(this.router);\n\n\
         \x20 /// The router every accessor below is bound to.\n  final DuetRouter router;\n\n\
         \x20 /// `{type_name}` itself, as one value at {}.\n\
         \x20 {handle}<{type_name}> get self =>\n      {handle}<{type_name}>(router, '{path}', const {type_name}Codec());\n",
        describe(path),
        describe(path)
    ));
    for accessor in accessors {
        out.push('\n');
        emit_accessor(out, type_name, accessor);
    }
    out.push_str("}\n");
}

/// One field's accessor: a nested client for a struct, a typed field otherwise.
fn emit_accessor(out: &mut String, type_name: &str, accessor: &PlannedAccessor) {
    let PlannedAccessor {
        key,
        accessor: name,
        path,
        ty,
        nested,
    } = accessor;
    if let Some(nested) = nested {
        // The struct's own handle lives on the nested class as `self`, so one
        // name serves both "the whole thing" and "its parts".
        out.push_str(&format!(
            "  /// `{type_name}.{key}`'s own accessors, at `{path}`.\n\
             \x20 {nested} get {name} => {nested}(router);\n"
        ));
        return;
    }
    let inner = dart_inner_ty(&ty.inner);
    let handle = if ty.optional {
        "DuetOptionalField"
    } else {
        "DuetField"
    };
    out.push_str(&format!(
        "  /// `{type_name}.{key}` at `{path}`.\n\
         \x20 {handle}<{inner}> get {name} =>\n      {handle}<{inner}>(router, '{path}', {});\n",
        dart_codec(&ty.inner)
    ));
}

/// The Dart type of a field, including its nullability.
fn dart_ty(ty: &PlannedTy) -> String {
    let inner = dart_inner_ty(&ty.inner);
    if ty.optional {
        format!("{inner}?")
    } else {
        inner
    }
}

/// The Dart type inside the option, or the whole type when there is none.
///
/// Recursive, and bounded: a [`Schema`](duet_schema::Schema) whose declared
/// nesting passes `MAX_VALUE_DEPTH` is refused before it reaches an emitter, so
/// this cannot be driven deeper than the store itself accepts.
fn dart_inner_ty(ty: &Ty) -> String {
    match ty {
        Ty::Bool => "bool".to_string(),
        Ty::Int => "int".to_string(),
        Ty::Float => "double".to_string(),
        Ty::Str => "String".to_string(),
        Ty::Bytes => "List<int>".to_string(),
        Ty::Dynamic => "DuetValue".to_string(),
        Ty::List(item) => format!("List<{}>", dart_inner_ty(item)),
        Ty::Map(value) => format!("Map<String, {}>", dart_inner_ty(value)),
        Ty::Named(name) => name.clone(),
        // Unreachable: `Plan::build` rejects an unknown arm before this runs,
        // and `Ty::Optional` is lifted out into `PlannedTy::optional`.
        _ => "Never".to_string(),
    }
}

/// The Dart expression for a codec of `ty`.
fn dart_codec(ty: &Ty) -> String {
    match ty {
        Ty::Bool => "duetBoolCodec".to_string(),
        Ty::Int => "duetIntCodec".to_string(),
        Ty::Float => "duetFloatCodec".to_string(),
        Ty::Str => "duetStringCodec".to_string(),
        Ty::Bytes => "duetBytesCodec".to_string(),
        Ty::Dynamic => "duetDynamicCodec".to_string(),
        Ty::List(item) => format!(
            "duetListCodec<{}>({})",
            dart_inner_ty(item),
            dart_codec(item)
        ),
        Ty::Map(value) => format!(
            "duetMapCodec<{}>({})",
            dart_inner_ty(value),
            dart_codec(value)
        ),
        Ty::Named(name) => format!("const {name}Codec()"),
        _ => "duetDynamicCodec".to_string(),
    }
}

/// `a` or `an`, so the generated doc comments read as English.
fn an(name: &str) -> String {
    let article = match name.chars().next() {
        Some(first) if "AEIOUaeiou".contains(first) => "an",
        _ => "a",
    };
    format!("{article} `{name}`")
}

/// How a wire path is described in a doc comment.
fn describe(path: &str) -> String {
    if path.is_empty() {
        "the store's root".to_string()
    } else {
        format!("`{path}`")
    }
}

#[cfg(test)]
#[path = "dart_tests.rs"]
mod tests;
