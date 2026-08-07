//! What a command body is handed besides its arguments: [`CommandContext`].

use duet_runtime::StoreHandle;

/// The host state one command invocation may reach.
///
/// A `#[command]` function takes `&CommandContext` as a parameter when it needs
/// the store, and omits it when it does not:
///
/// ```text
/// #[command] fn add(a: i64, b: i64) -> i64 { a + b }
/// #[command] fn reset(ctx: &CommandContext) -> Result<(), MyError> { … }
/// ```
///
/// # Why a wrapper rather than a bare `&StoreHandle`
///
/// Two reasons, and neither is decoration.
///
/// The parameter has to be **distinguishable from an argument**, and that
/// distinction must not be a name the macro matches on. `#[command]` decides by
/// *binding mode* — a parameter written as a reference is the context, a
/// parameter written by value is an argument — and then lets trait resolution
/// decide whether the referent is legal, through [`FromContext`]. A bare
/// `&StoreHandle` would work identically today and would leave nothing to hang
/// a second capability on tomorrow, at which point every command signature in
/// every downstream crate would have to change.
///
/// And it is a **place to say no**. A context is the natural home for anything
/// a command may reach that the schema does not describe — the surface it was
/// called from, a cancellation flag, a clock — and each of those is a decision
/// to be made once, here, rather than by whichever handler reached for a global.
/// It holds exactly one thing today.
///
/// # It is per-invocation, and it clones a handle
///
/// One `CommandContext` is built per `invoke`, holding its own
/// [`StoreHandle`]. Cloning a handle is a channel-sender clone plus an atomic
/// pointer bump — the same cost every `Runtime::handle()` call already pays —
/// and it buys a type with no lifetime parameter, which is what lets a command
/// signature be written `&CommandContext` rather than `&CommandContext<'_>` and
/// what keeps [`CommandEntry`](crate::CommandEntry)'s `run` a plain function
/// pointer rather than a higher-ranked one.
#[derive(Debug, Clone)]
pub struct CommandContext {
    store: StoreHandle,
}

impl CommandContext {
    /// Builds the context for one invocation.
    ///
    /// Called by [`Commands`](crate::Commands) once per `invoke`; a command body
    /// receives one and does not construct one.
    #[must_use]
    pub fn new(store: StoreHandle) -> CommandContext {
        CommandContext { store }
    }

    /// The shared store, which a body may read and write freely.
    ///
    /// A handler runs on the thread that called `dispatch_with` — never the
    /// runtime's core thread — which is exactly why this works. Every word of
    /// `duet_protocol::CommandHost`'s threading contract applies: use it, do not
    /// block on it.
    #[must_use]
    pub fn store(&self) -> &StoreHandle {
        &self.store
    }
}

/// How a `#[command]`'s reference parameter is produced from the context.
///
/// # This is the type check behind a syntactic decision
///
/// `#[command]` reads a parameter's *shape*: written as a reference, it is the
/// context; written by value, it is an argument decoded from `args`. That much
/// is syntax, and it has to be — a macro sees tokens and never resolved types,
/// so nothing else can tell the two binding modes apart.
///
/// What the macro does **not** do is decide what the reference points at. It
/// emits `<&Whatever as FromContext>::from_context(ctx)` and lets the compiler
/// answer, which makes both failure directions safe:
///
/// - `type Ctx = CommandContext; fn f(c: &Ctx)` — the alias resolves to
///   `&CommandContext`, the impl applies, and the command works. Token
///   inspection would have refused it.
/// - `fn f(s: &str)` — no impl, so it is a compile error naming this trait.
/// - `type CtxRef<'a> = &'a CommandContext; fn f(c: CtxRef<'_>)` — not written
///   as a reference, so it is treated as an argument, and `CtxRef` is not
///   `SharedState`. Also a compile error.
///
/// Every wrong spelling fails closed, at `cargo check`, naming a trait the
/// developer can look up.
#[diagnostic::on_unimplemented(
    message = "a `#[command]` parameter written as a reference must be `&CommandContext`",
    label = "`{Self}` is not how a command receives its context",
    note = "A command takes its arguments **by value** and its context **by reference**:\n\
            \x20 `#[command] fn add(a: i64, b: i64) -> i64`\n\
            \x20 `#[command] fn reset(ctx: &CommandContext) -> Result<(), MyError>`\n\
            The fixes:\n\
            \x20 to read or write the store, take `&CommandContext` and use `ctx.store()`\n\
            \x20 to take this as an argument, take it by value — `&str` becomes `String`, \
            `&[T]` becomes `Vec<T>` (the store owns a `'static` tree, so a command cannot \
            borrow from it)"
)]
pub trait FromContext<'a>: Sized {
    /// Produces this parameter from the invocation's context.
    fn from_context(context: &'a CommandContext) -> Self;
}

impl<'a> FromContext<'a> for &'a CommandContext {
    fn from_context(context: &'a CommandContext) -> &'a CommandContext {
        context
    }
}

#[cfg(test)]
#[path = "context_tests.rs"]
mod tests;
