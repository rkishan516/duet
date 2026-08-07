//! One command as a value: [`Command`], [`CommandEntry`] and [`commands!`].

use duet_protocol::{Args, Outcome};
use duet_schema::{CommandDef, Registry};

use crate::context::CommandContext;

/// One command, as `#[command]` writes it.
///
/// A user never implements this by hand — `#[command]` generates the impl next
/// to the function it is written on, on a hidden type of the same name. It is
/// public because generated code has to name it, and documented because a
/// developer reading an expansion deserves to know what they are looking at.
///
/// # The two halves have to be one impl
///
/// [`describe`](Command::describe) is what the schema says the command is;
/// [`run`](Command::run) is what actually happens. A generated Dart client is
/// built against the first and talks to the second, so a command whose two
/// halves disagreed would be a client that compiles and cannot call anything.
/// Keeping them on one trait means one macro expansion produces both, from one
/// function signature, and there is no second place for either to drift to.
pub trait Command {
    /// The name a guest invokes.
    ///
    /// An associated `const` rather than a method, so
    /// [`CommandEntry::of`] can read it in a `const` context and a whole
    /// registry can be a `static` with no runtime construction at all.
    const NAME: &'static str;

    /// How the schema describes this command.
    ///
    /// Takes the registry the rest of the schema is being built into, because a
    /// parameter or a return type may be a struct that has to appear in
    /// `types`.
    fn describe(registry: &mut Registry) -> CommandDef;

    /// Runs the command against one invocation's arguments.
    fn run(args: Args, context: &CommandContext) -> Outcome;
}

/// A [`Command`] with its type erased, so a set of them can be one array.
///
/// # A `static`, with nothing built at run time
///
/// [`of`](CommandEntry::of) is a `const fn`, and everything it reads is
/// `const`-evaluable: `C::NAME` is an associated constant and the two function
/// pointers are coercions of `const`-known function items. So
///
/// ```text
/// static COMMANDS: [CommandEntry; 3] = commands![subtract, raise, bump];
/// ```
///
/// is a table in the binary's read-only data. There is no initialization order
/// to reason about, no `OnceLock`, no allocation, and a registry built from it
/// cannot fail — which matters because the alternative shape, a `Vec` assembled
/// at startup, is one more thing that can be assembled twice or not at all.
#[derive(Debug, Clone, Copy)]
pub struct CommandEntry {
    name: &'static str,
    describe: fn(&mut Registry) -> CommandDef,
    run: fn(Args, &CommandContext) -> Outcome,
}

impl CommandEntry {
    /// Erases `C` into an entry.
    ///
    /// `const`, deliberately — see the type's documentation.
    #[must_use]
    pub const fn of<C: Command>() -> CommandEntry {
        CommandEntry {
            name: C::NAME,
            describe: C::describe,
            run: C::run,
        }
    }

    /// The name a guest invokes.
    #[must_use]
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// How the schema describes this command.
    #[must_use]
    pub fn describe(&self, registry: &mut Registry) -> CommandDef {
        (self.describe)(registry)
    }

    /// Runs the command.
    #[must_use]
    pub fn run(&self, args: Args, context: &CommandContext) -> Outcome {
        (self.run)(args, context)
    }
}

/// Describes every entry into one registry, for [`Schema::of_with_commands`].
///
/// ```text
/// let schema = Schema::of_with_commands::<App>(|registry| describe(&COMMANDS, registry))?;
/// ```
///
/// The registry is shared with the root's own description, which is the whole
/// point: a command returning `Editor` renders as `{"kind": "named", …}` and
/// `Editor` lands in `types`, whether or not any field of the root reaches it.
///
/// [`Schema::of_with_commands`]: duet_schema::Schema::of_with_commands
#[must_use]
pub fn describe(entries: &[CommandEntry], registry: &mut Registry) -> Vec<CommandDef> {
    entries
        .iter()
        .map(|entry| entry.describe(registry))
        .collect()
}

/// Builds a `static` array of [`CommandEntry`] from a list of `#[command]`
/// functions.
///
/// ```text
/// use duet::{CommandEntry, commands};
///
/// #[command] fn add(a: i64, b: i64) -> i64 { a + b }
/// #[command] fn reset(ctx: &CommandContext) -> Result<(), MyError> { … }
///
/// static COMMANDS: [CommandEntry; 2] = commands![add, reset];
/// ```
///
/// # Why it names the functions and not some generated symbol
///
/// `#[command]` generates a hidden type with the **same name** as the function
/// it is written on. A function lives in the value namespace and a braced
/// `struct add {}` lives only in the type namespace, so the two coexist and one
/// `use crate::commands::add;` brings both. A list of function names is
/// therefore also a list of type names, and an embedder's registration reads as
/// the list of things it is registering rather than as a list of macro
/// artefacts.
#[macro_export]
macro_rules! commands {
    ($($name:path),* $(,)?) => {
        [$($crate::CommandEntry::of::<$name>()),*]
    };
}

#[cfg(test)]
#[path = "entry_tests.rs"]
mod tests;
