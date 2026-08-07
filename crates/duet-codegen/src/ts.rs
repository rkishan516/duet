//! The TypeScript emitter.
//!
//! Mirrors `dart.rs` declaration for declaration. The one structural difference
//! is the import block: `packages/duet-js` compiles with `noUnusedLocals` and
//! `verbatimModuleSyntax`, so a generated file must import **exactly** the
//! symbols it uses and must separate type-only imports from value ones. The
//! emitter therefore records what it used while writing the body and writes the
//! imports afterwards, from a sorted set — an unused import here is a build
//! failure, not a lint.

use std::collections::BTreeSet;

use duet_schema::Ty;

use crate::command::PlannedCommand;
use crate::emit::{Options, header};
use crate::name::lower_camel;
use crate::plan::{
    Plan, PlannedAccessor, PlannedClass, PlannedField, PlannedTy, PlannedType, commands_class_name,
};

/// The body under construction, plus the runtime symbols it has reached for.
#[derive(Default)]
struct Body {
    text: String,
    /// Values imported from the package root.
    root_values: BTreeSet<&'static str>,
    /// Types imported from the package root.
    root_types: BTreeSet<&'static str>,
    /// Values imported from the typed runtime.
    typed_values: BTreeSet<&'static str>,
    /// Types imported from the typed runtime.
    typed_types: BTreeSet<&'static str>,
}

impl Body {
    /// Appends `text`.
    fn push(&mut self, text: &str) {
        self.text.push_str(text);
    }

    /// Records a value imported from the typed runtime and returns its name, so
    /// a use site and its import cannot drift apart.
    fn typed_value(&mut self, name: &'static str) -> &'static str {
        self.typed_values.insert(name);
        name
    }
}

/// Emits the whole TypeScript client.
pub(crate) fn emit(plan: &Plan, options: &Options) -> String {
    let mut body = Body::default();
    for planned in &plan.types {
        body.push("\n");
        emit_interface(&mut body, planned);
        body.push("\n");
        emit_codec(&mut body, planned);
    }
    for class in &plan.classes {
        body.push("\n");
        emit_client(&mut body, class);
    }
    // Nothing at all for a schema that declares no commands, so such a schema
    // generates byte-for-byte what it generated before commands existed —
    // including its import block, which is written from what the body used.
    if !plan.commands.is_empty() {
        body.push("\n");
        emit_commands(&mut body, plan);
    }

    let mut out = header(options, "TypeScript");
    out.push_str(&format!(
        "\n/**\n * The typed Duet client generated from `{}`.\n *\n * @module\n */\n",
        options.source
    ));
    out.push_str(&imports(&body, options));
    out.push_str(&body.text);
    out
}

/// The import block, written after the body so it names exactly what was used.
fn imports(body: &Body, options: &Options) -> String {
    let mut out = String::new();
    for (module, values, types) in [
        (
            &options.ts_imports.root,
            &body.root_values,
            &body.root_types,
        ),
        (
            &options.ts_imports.typed,
            &body.typed_values,
            &body.typed_types,
        ),
    ] {
        let named = values
            .iter()
            .map(|name| (*name).to_string())
            .chain(types.iter().map(|name| format!("type {name}")))
            .collect::<Vec<_>>();
        if !named.is_empty() {
            out.push_str(&format!(
                "\nimport {{\n{}}} from '{module}';\n",
                named
                    .iter()
                    .map(|entry| format!("  {entry},\n"))
                    .collect::<String>()
            ));
        }
    }
    out
}

/// The commands class: one method per command, each a literal name, a literal
/// key per argument, and a delegation to the hand-written `invoke`.
fn emit_commands(body: &mut Body, plan: &Plan) {
    let name = commands_class_name(&plan.root);
    body.root_types.insert("DuetClient");
    body.push(&format!(
        "/**\n * The commands `{}` declares, as typed methods.\n *\n\
         \x20* Every command name and every argument key here is a literal; see this\n\
         \x20* file's header. A method resolves with what the command *answered*; a host\n\
         \x20* that refused to run it rejects with a `DuetFailure` from\n\
         \x20* `DuetClient.invoke`.\n */\n\
         export class {name} {{\n\
         \x20 /** The client every command below is invoked through. */\n  readonly client: DuetClient;\n\n\
         \x20 /** Binds these commands to `client`. */\n  constructor(client: DuetClient) {{\n    this.client = client;\n  }}\n",
        plan.root
    ));
    for command in &plan.commands {
        body.push("\n");
        emit_command(body, command);
    }
    body.push("}\n");
}

/// One command's method.
fn emit_command(body: &mut Body, command: &PlannedCommand) {
    let returns = ts_inner_ty(body, &command.returns);
    let raises = ts_inner_ty(body, &command.raises);
    body.typed_types.insert("DuetOutcome");
    let decode = body.typed_value("duetDecodeOutcome");
    body.push(&format!(
        "  /**\n   * Invokes `{}`.\n   *\n   * {}\n   * {}\n   */\n",
        command.name,
        describe_result("Returns", command.declares_return, &returns),
        describe_result("Raises", command.declares_raise, &raises),
    ));
    let members: Vec<String> = command
        .params
        .iter()
        .map(|param| {
            let ty = ts_inner_ty(body, &param.ty);
            format!("readonly {}: {ty};", param.accessor)
        })
        .collect();
    let signature = if members.is_empty() {
        String::new()
    } else {
        format!(
            "params: {{\n{}  }}",
            members
                .iter()
                .map(|member| format!("    {member}\n"))
                .collect::<String>()
        )
    };
    body.push(&format!(
        "  async {}({signature}): Promise<DuetOutcome<{returns}, {raises}>> {{\n    return {decode}<{returns}, {raises}>(\n",
        command.method
    ));
    emit_invocation(body, command);
    let return_codec = ts_codec(body, &command.returns);
    let raise_codec = ts_codec(body, &command.raises);
    body.push(&format!(
        "      {return_codec},\n      {raise_codec},\n    );\n  }}\n"
    ));
}

/// The `this.client.invoke(...)` call, with the argument map in wire-key order.
fn emit_invocation(body: &mut Body, command: &PlannedCommand) {
    let name = &command.name;
    if command.params.is_empty() {
        body.push(&format!("      await this.client.invoke('{name}'),\n"));
        return;
    }
    body.root_types.insert("DuetValue");
    // The name stays on the same line as `invoke(` in both languages, so a
    // reviewer — and `crates/duet-host-stdio/tests/commands.rs`, which resolves
    // every one of them against a live registry — can grep one pattern for the
    // exact wire string rather than two.
    body.push(&format!(
        "      await this.client.invoke('{name}', new Map<string, DuetValue>([\n"
    ));
    for param in command.sorted_params() {
        let codec = ts_codec(body, &param.ty);
        body.push(&format!(
            "        ['{}', {codec}.encode(params.{})],\n",
            param.key, param.accessor
        ));
    }
    body.push("      ])),\n");
}

/// One line of a command method's doc comment.
///
/// A command that declares no type at that position still *answers* there —
/// with `null` — so the generated doc says so rather than leaving a reader to
/// infer it from a `DuetValue` in the signature.
fn describe_result(what: &str, declared: bool, ty: &str) -> String {
    if declared {
        format!("{what} `{ty}`.")
    } else {
        format!("{what} nothing; the schema declares no type, so this is the raw value.")
    }
}

/// `export interface Editor { … }`.
fn emit_interface(body: &mut Body, planned: &PlannedType) {
    let name = &planned.name;
    body.push(&format!(
        "/** `{name}`, as the schema describes it. */\nexport interface {name} {{\n"
    ));
    for field in &planned.fields {
        let ty = ts_ty(body, &field.ty);
        body.push(&format!(
            "  /** The `{}` field. */\n  readonly {}: {ty};\n",
            field.key, field.accessor
        ));
    }
    body.push("}\n");
}

/// `export const editorCodec: DuetCodec<Editor> = { … }`.
fn emit_codec(body: &mut Body, planned: &PlannedType) {
    let name = &planned.name;
    let constant = codec_constant(name);
    body.typed_types.insert("DuetCodec");
    body.push(&format!(
        "/** The codec for {{@link {name}}}. */\nexport const {constant}: DuetCodec<{name}> = {{\n\
         \x20 name: '{name}',\n"
    ));
    emit_encode(body, planned);
    emit_decode(body, planned);
    body.push("};\n");
}

/// The encoder: one map entry per field, each delegating to a codec.
fn emit_encode(body: &mut Body, planned: &PlannedType) {
    body.root_values.insert("duetMap");
    body.root_types.insert("DuetValue");
    body.push("  encode: (value) =>\n    duetMap(\n      new Map<string, DuetValue>([\n");
    for field in &planned.fields {
        let codec = ts_codec(body, &field.ty.inner);
        let expression = if field.ty.optional {
            body.root_values.insert("duetNull");
            format!(
                "value.{0} === null ? duetNull() : {codec}.encode(value.{0})",
                field.accessor
            )
        } else {
            format!("{codec}.encode(value.{})", field.accessor)
        };
        body.push(&format!("        ['{}', {expression}],\n", field.key));
    }
    body.push("      ]),\n    ),\n");
}

/// The decoder: one reading per field, then the object literal.
///
/// Every field goes through `duetRequiredReading` or `duetOptionalReading` — the
/// same two hand-written, hand-tested functions a field's `get` uses — so the
/// absent / null / mismatch distinctions inside a struct are exactly the ones at
/// the top level rather than a second implementation of them.
fn emit_decode(body: &mut Body, planned: &PlannedType) {
    body.push("  decode: (value) => {\n    if (value.kind !== 'map') return null;\n");
    for field in &planned.fields {
        let codec = ts_codec(body, &field.ty.inner);
        let reader = if field.ty.optional {
            body.typed_value("duetOptionalReading")
        } else {
            body.typed_value("duetRequiredReading")
        };
        body.push(&format!(
            "    const {} = {reader}(\n      {codec},\n      value.entries.get('{}') ?? null,\n    );\n",
            field.accessor, field.key
        ));
        if field.ty.optional {
            body.push(&format!(
                "    if ({0}.kind === 'absent' || {0}.kind === 'mismatch') return null;\n",
                field.accessor
            ));
        } else {
            body.push(&format!(
                "    if ({}.kind !== 'present') return null;\n",
                field.accessor
            ));
        }
    }
    if planned.fields.iter().any(|f| f.ty.optional) {
        body.typed_value("duetReadingValue");
    }
    let members: Vec<String> = planned.fields.iter().map(ts_member).collect();
    body.push(&format!(
        "    return {};\n  }},\n",
        listed(&members, "    ")
    ));
}

/// The most members that stay on one line.
///
/// A fixed count rather than a measured line width, so the wrapping is a
/// function of the schema alone: a golden must not change because a field was
/// renamed by one character. The same limit `dart.rs` uses.
const INLINE_LIMIT: usize = 3;

/// An object literal — on one line while it is short, one member per line and
/// trailing-comma'd once it is not.
fn listed(members: &[String], indent: &str) -> String {
    if members.len() <= INLINE_LIMIT {
        return format!("{{ {} }}", members.join(", "));
    }
    let body = members
        .iter()
        .map(|member| format!("{indent}  {member},\n"))
        .collect::<String>();
    format!("{{\n{body}{indent}}}")
}

/// How one decoded reading becomes an object member.
fn ts_member(field: &PlannedField) -> String {
    if field.ty.optional {
        // `duetReadingValue` is null for both a none reading and the arms
        // already rejected above, so by here it means exactly `Option::None`.
        format!("{}: duetReadingValue({})", field.accessor, field.accessor)
    } else {
        format!("{0}: {0}.value", field.accessor)
    }
}

/// One accessor class: the router, the struct's own handle, and one accessor per
/// field.
fn emit_client(body: &mut Body, class: &PlannedClass) {
    let PlannedClass {
        name,
        type_name,
        path,
        optional,
        accessors,
    } = class;
    body.typed_types.insert("DuetRouter");
    // An `Option<Editor>` reached through this class must answer *none* rather
    // than a mismatch, so the struct's own handle follows the field that
    // reached it.
    let handle = if *optional {
        body.typed_value("DuetOptionalField")
    } else {
        body.typed_value("DuetField")
    };
    let codec = codec_constant(type_name);
    body.push(&format!(
        "/**\n * Typed accessors for `{type_name}` at {}.\n *\n\
         \x20* Every path here is a literal; see this file's header.\n */\n\
         export class {name} {{\n\
         \x20 /** The router every accessor below is bound to. */\n  readonly router: DuetRouter;\n\n\
         \x20 /** Binds these accessors to `router`. */\n  constructor(router: DuetRouter) {{\n    this.router = router;\n  }}\n\n\
         \x20 /** `{type_name}` itself, as one value at {}. */\n\
         \x20 get self(): {handle}<{type_name}> {{\n    return new {handle}<{type_name}>(this.router, '{path}', {codec});\n  }}\n",
        describe(path),
        describe(path)
    ));
    for accessor in accessors {
        body.push("\n");
        emit_accessor(body, type_name, accessor);
    }
    body.push("}\n");
}

/// One field's accessor: a nested client for a struct, a typed field otherwise.
fn emit_accessor(body: &mut Body, type_name: &str, accessor: &PlannedAccessor) {
    let PlannedAccessor {
        key,
        accessor: name,
        path,
        ty,
        nested,
    } = accessor;
    if let Some(nested) = nested {
        body.push(&format!(
            "  /** `{type_name}.{key}`'s own accessors, at `{path}`. */\n\
             \x20 get {name}(): {nested} {{\n    return new {nested}(this.router);\n  }}\n"
        ));
        return;
    }
    let handle = if ty.optional {
        body.typed_value("DuetOptionalField")
    } else {
        body.typed_value("DuetField")
    };
    let inner = ts_inner_ty(body, &ty.inner);
    let codec = ts_codec(body, &ty.inner);
    body.push(&format!(
        "  /** `{type_name}.{key}` at `{path}`. */\n\
         \x20 get {name}(): {handle}<{inner}> {{\n\
         \x20   return new {handle}<{inner}>(this.router, '{path}', {codec});\n  }}\n"
    ));
}

/// The TypeScript type of a field, including its nullability.
fn ts_ty(body: &mut Body, ty: &PlannedTy) -> String {
    let inner = ts_inner_ty(body, &ty.inner);
    if ty.optional {
        format!("{inner} | null")
    } else {
        inner
    }
}

/// The TypeScript type inside the option, or the whole type when there is none.
///
/// Recursive, and bounded: a [`Schema`](duet_schema::Schema) whose declared
/// nesting passes `MAX_VALUE_DEPTH` is refused before it reaches an emitter.
fn ts_inner_ty(body: &mut Body, ty: &Ty) -> String {
    match ty {
        Ty::Bool => "boolean".to_string(),
        Ty::Int => "bigint".to_string(),
        Ty::Float => "number".to_string(),
        Ty::Str => "string".to_string(),
        Ty::Bytes => "Uint8Array".to_string(),
        Ty::Dynamic => {
            body.root_types.insert("DuetValue");
            "DuetValue".to_string()
        }
        Ty::List(item) => format!("{}[]", ts_inner_ty(body, item)),
        Ty::Map(value) => format!("Map<string, {}>", ts_inner_ty(body, value)),
        Ty::Named(name) => name.clone(),
        // Unreachable: `Plan::build` rejects an unknown arm before this runs,
        // and `Ty::Optional` is lifted out into `PlannedTy::optional`.
        _ => "never".to_string(),
    }
}

/// The TypeScript expression for a codec of `ty`.
fn ts_codec(body: &mut Body, ty: &Ty) -> String {
    match ty {
        Ty::Bool => body.typed_value("duetBoolCodec").to_string(),
        Ty::Int => body.typed_value("duetIntCodec").to_string(),
        Ty::Float => body.typed_value("duetFloatCodec").to_string(),
        Ty::Str => body.typed_value("duetStringCodec").to_string(),
        Ty::Bytes => body.typed_value("duetBytesCodec").to_string(),
        Ty::Dynamic => body.typed_value("duetDynamicCodec").to_string(),
        Ty::List(item) => {
            let factory = body.typed_value("duetListCodec");
            let inner = ts_inner_ty(body, item);
            format!("{factory}<{inner}>({})", ts_codec(body, item))
        }
        Ty::Map(value) => {
            let factory = body.typed_value("duetMapCodec");
            let inner = ts_inner_ty(body, value);
            format!("{factory}<{inner}>({})", ts_codec(body, value))
        }
        Ty::Named(name) => codec_constant(name),
        _ => body.typed_value("duetDynamicCodec").to_string(),
    }
}

/// The name of a schema type's codec constant.
fn codec_constant(type_name: &str) -> String {
    format!("{}Codec", lower_camel(type_name))
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
#[path = "ts_tests.rs"]
mod tests;
