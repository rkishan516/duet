//! The command half of the emit plan: every method name, parameter name and
//! codec, decided once for both emitters.
//!
//! The state half lives in [`plan`](crate::plan) and this reads exactly like
//! it, because the hazards are the same ones. A command name is a **wire**
//! string: it is what `duet_command::Commands` looks up on the host, and a
//! generated client that camel-cased it would produce
//! `client.invoke('documentsClose')` against a host holding
//! `documents.close` — no compile error, no runtime error, just a refusal at
//! the far end of a call the developer believed was typed. So the same rule
//! applies: **a wire name is never rewritten; a method name always is.**
//!
//! Parameter keys are the same shape of hazard one level down. They are keys in
//! the `args` map the host decodes by name, so `font_size` stays `font_size` on
//! the wire and becomes `fontSize` in the signature.

use duet_schema::{CommandDef, Ty};

use crate::error::EmitError;
use crate::name::{command_method, is_emittable_key, is_usable_member, lower_camel};
use crate::plan::{check_supported, contains_optional};

/// One parameter of one command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedParam {
    /// The `args` key, **exactly** as the schema spells it.
    pub key: String,
    /// The parameter name in both generated signatures: `lowerCamelCase`.
    pub accessor: String,
    /// What the parameter holds. Never an `optional` — see [`plan_ty`].
    ///
    /// [`plan_ty`]: crate::plan
    pub ty: Ty,
}

/// One command, named for both target languages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedCommand {
    /// The name a guest invokes, **exactly** as the schema spells it, dots and
    /// all.
    pub name: String,
    /// The generated method name: `lowerCamelCase`, dots collapsed like
    /// underscores. Never appears on the wire.
    pub method: String,
    /// The parameters, in **declaration order** — the order a reader of the
    /// Rust function sees, and so the order the generated signature lists.
    pub params: Vec<PlannedParam>,
    /// What a `returned` reply carries.
    ///
    /// [`Ty::Dynamic`] when the schema declares no `returns`, because the wire
    /// still answers — with `Value::Null` — and `dynamic` is the one type that
    /// describes "whatever arrives" without inventing an arm for it. See
    /// [`declares_return`](PlannedCommand::declares_return) for telling the two
    /// apart.
    pub returns: Ty,
    /// What a `raised` reply carries; [`Ty::Dynamic`] when none is declared.
    pub raises: Ty,
    /// True when the schema declared a `returns` type, rather than this being
    /// the `dynamic` stand-in.
    pub declares_return: bool,
    /// True when the schema declared a `raises` type.
    pub declares_raise: bool,
}

impl PlannedCommand {
    /// The parameters in **byte order of their wire keys**.
    ///
    /// What the generated `args` literal is written in, so the request bytes a
    /// guest produces are canonical before any encoder sorts them. The wire
    /// format orders a map's keys by their UTF-8 bytes
    /// (`duet_codec`'s canonical encoding), and a generated literal that
    /// listed them in declaration order would rely on every guest runtime
    /// sorting on the way out — which two of the three do, and which no test
    /// of the *generated* code would notice the day one stopped.
    pub fn sorted_params(&self) -> Vec<&PlannedParam> {
        let mut sorted: Vec<&PlannedParam> = self.params.iter().collect();
        sorted.sort_by(|a, b| a.key.as_bytes().cmp(b.key.as_bytes()));
        sorted
    }
}

/// Plans every command in the schema, or says why one cannot be spelled.
///
/// # Errors
///
/// [`EmitError`] for a command name or parameter key with no faithful spelling
/// in Dart and TypeScript, two commands wanting one method name, two parameters
/// wanting one name, or a signature type this emitter cannot bind a codec for.
pub fn plan_commands(commands: &[CommandDef]) -> Result<Vec<PlannedCommand>, EmitError> {
    let mut planned: Vec<PlannedCommand> = Vec::with_capacity(commands.len());
    for command in commands {
        let next = plan_command(command)?;
        if planned.iter().any(|done| done.method == next.method) {
            return Err(EmitError::CommandCollision {
                method: next.method,
            });
        }
        planned.push(next);
    }
    Ok(planned)
}

/// Plans one command.
fn plan_command(command: &CommandDef) -> Result<PlannedCommand, EmitError> {
    let method = command_method(&command.name);
    if !is_usable_member(&method) {
        return Err(EmitError::UnspellableCommand {
            command: command.name.clone(),
            because: "its name does not become a Dart and TypeScript identifier \
                      the emitter has not already claimed",
        });
    }
    let params = plan_params(command)?;
    Ok(PlannedCommand {
        name: command.name.clone(),
        method,
        params,
        returns: signature_ty(command, command.returns.as_ref(), "returns")?,
        raises: signature_ty(command, command.raises.as_ref(), "raises")?,
        declares_return: command.returns.is_some(),
        declares_raise: command.raises.is_some(),
    })
}

/// Names and checks every parameter of one command.
fn plan_params(command: &CommandDef) -> Result<Vec<PlannedParam>, EmitError> {
    let mut params: Vec<PlannedParam> = Vec::with_capacity(command.params.len());
    for param in &command.params {
        if !is_emittable_key(&param.key) {
            return Err(EmitError::UnspellableCommandType {
                command: command.name.clone(),
                what: param.key.clone(),
                because: "a generated parameter key must be ASCII letters, digits \
                          or underscores",
            });
        }
        let accessor = lower_camel(&param.key);
        if !is_usable_member(&accessor) {
            return Err(EmitError::UnspellableCommandType {
                command: command.name.clone(),
                what: param.key.clone(),
                because: "it does not name a Dart and TypeScript identifier the \
                          emitter has not already claimed",
            });
        }
        if params.iter().any(|done| done.accessor == accessor) {
            return Err(EmitError::ParamCollision {
                command: command.name.clone(),
                accessor,
            });
        }
        params.push(PlannedParam {
            key: param.key.clone(),
            accessor,
            ty: checked(command, &param.ty, &param.key)?,
        });
    }
    Ok(params)
}

/// The type at one position of a signature, or [`Ty::Dynamic`] when the schema
/// declares none there.
fn signature_ty(
    command: &CommandDef,
    declared: Option<&Ty>,
    what: &'static str,
) -> Result<Ty, EmitError> {
    match declared {
        None => Ok(Ty::Dynamic),
        Some(ty) => checked(command, ty, what),
    }
}

/// Checks one signature type can be bound to a codec, and returns it.
///
/// # Why an `optional` is refused here rather than lifted
///
/// [`plan_ty`](crate::plan) lifts a struct field's optionality out because the
/// runtime has a second handle for it — `DuetOptionalField` beside `DuetField`.
/// A command signature has no such pair: an argument is encoded through one
/// codec and a result is decoded through one codec, and `DuetCodec`'s type
/// argument is non-nullable by design, which is what keeps "decoded to null"
/// from colliding with "refused". So an `optional` anywhere in a command's
/// signature is refused by name, with the fix stated — exactly as a misplaced
/// optional inside a container already is.
fn checked(command: &CommandDef, ty: &Ty, what: &str) -> Result<Ty, EmitError> {
    if contains_optional(ty) {
        return Err(EmitError::UnspellableCommandType {
            command: command.name.clone(),
            what: what.to_string(),
            because: "a command's types are bound to codecs, whose type argument is \
                      non-nullable; wrap the optional in a struct field, which has a \
                      second handle for it",
        });
    }
    check_supported(ty, &command.name, what).map_err(|_| EmitError::UnspellableCommandType {
        command: command.name.clone(),
        what: what.to_string(),
        because: "it uses a schema type this emitter has no spelling for; the \
                  emitter needs updating",
    })?;
    Ok(ty.clone())
}

#[cfg(test)]
#[path = "command_tests.rs"]
mod tests;
