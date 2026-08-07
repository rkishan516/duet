//! The generated code compiles where nothing it needs is in scope, and where
//! every name it might have reached for means something else.
//!
//! # Measured, not assumed
//!
//! Hygiene is the property of a macro that is easiest to believe and hardest to
//! notice losing: an unqualified `Ok(...)` works in every crate that has not
//! shadowed `Ok`, which is every crate anyone tries it in. The module below
//! shadows all eight of the names the generated code could have reached for,
//! puts a `mod duet` in the way of the crate's own name, and then derives.
//!
//! If any path in `generate.rs` stops being absolute, this file stops
//! compiling — and a test that does not compile is a failure, not a skip.

use duet::runtime::NullSink;
use duet::{Reading, Runtime, Schema, SharedState, Value};

/// A crate that re-exports Duet under its own name, reduced to a module.
///
/// It re-exports exactly the eight names `#[duet(crate = ...)]` documents as
/// its contract and nothing else, so that list is checked rather than merely
/// described. Adding a ninth path to the generated code fails here.
mod my_reexport {
    pub use duet::{
        DecodeError, FieldDef, NotNullable, Registry, SharedState, SkippedDefault, Ty, Value,
    };
}

/// The same thing for `#[command(crate = ...)]`, whose contract is a different
/// and larger set of names.
mod my_command_reexport {
    pub use duet::{
        Args, Command, CommandContext, CommandDef, CommandParam, FieldDef, FromContext, Outcome,
        Registry, command_raises, command_returns, into_outcome,
    };
}

/// A module in which every prelude name the generated code could have used
/// means something else, and `duet` is not the crate.
mod hostile {
    #![allow(dead_code)]

    // The one thing a developer writes. It brings the derive and the trait into
    // scope under the name `SharedState`; it does **not** put `duet` in scope,
    // which is the point.
    use ::duet::SharedState;

    /// `duet` names this, not the crate. Every generated path starts `::duet`,
    /// which resolves in the extern prelude and cannot be reached by this.
    mod duet {}

    // The eight names the task named, each shadowed in both namespaces a unit
    // struct occupies. `Ok(x)` in this module is not a call to a variant — it
    // is an error, which is exactly what makes the check real.
    pub struct Result;
    pub struct Option;
    pub struct Ok;
    pub struct Err;
    pub struct Some;
    pub struct None;
    pub struct String;
    pub struct Vec;

    /// Not shared state, and not `Copy`, so `skipped_default` is doing work.
    #[derive(Debug, Default, PartialEq)]
    pub struct Cache(pub i64);

    /// A struct with fields, so the non-empty arms of every generated method
    /// are the ones compiled here.
    #[derive(Debug, PartialEq, SharedState)]
    pub struct Shadowed {
        pub counter: i64,
        pub ratio: f64,
        pub flag: bool,
        #[duet(rename = "renamed")]
        pub original: i64,
        #[duet(skip)]
        pub cache: Cache,
    }

    /// A struct with no shared fields, so the two empty arms — the map with no
    /// entries and the definition with no fields — are compiled too.
    #[derive(Debug, PartialEq, SharedState)]
    pub struct AllSkipped {
        #[duet(skip)]
        pub cache: Cache,
    }

    /// Named through the re-export rather than through `::duet`, in the same
    /// hostile scope.
    #[derive(Debug, PartialEq, SharedState)]
    #[duet(crate = crate::my_reexport)]
    pub struct ThroughAFacade {
        pub counter: i64,
        #[duet(skip)]
        pub cache: Cache,
    }

    // --- `#[command]`, in the same hostile scope ---
    //
    // The one thing a developer writes for a command. It does not put `duet`
    // in scope either, and every name below still means something else.
    use ::duet::command;

    /// Every argument shape at once — a scalar, a renamed one, a struct, and an
    /// option — plus the context, so every arm of `run` is compiled here.
    #[command]
    pub fn shadowed_subtract(
        ctx: &::duet::CommandContext,
        a: i64,
        #[duet(rename = "renamed")] b: i64,
        note: ::std::option::Option<::std::string::String>,
    ) -> ::core::result::Result<i64, Shadowed> {
        let _ = (ctx, note);
        ::core::result::Result::Ok(a - b)
    }

    /// No arguments, no context, no return type: the three empty arms.
    #[command]
    pub fn shadowed_nothing() {}

    /// Named through the re-export rather than through `::duet`, and taking the
    /// context, so every one of the eleven paths the re-export promises is
    /// reached through it.
    #[command(crate = crate::my_command_reexport, rename = "through.a.facade")]
    pub fn shadowed_through_a_facade(ctx: &::duet::CommandContext, a: i64) -> i64 {
        let _ = ctx;
        a
    }
}

#[test]
fn the_derived_impls_work_where_every_prelude_name_is_shadowed() {
    let original = hostile::Shadowed {
        counter: 7,
        ratio: 1.5,
        flag: true,
        original: 3,
        cache: hostile::Cache(99),
    };
    let lowered = original.to_value();
    assert_eq!(
        lowered,
        Value::map([
            ("counter", Value::Int(7)),
            ("ratio", Value::Float(1.5)),
            ("flag", Value::Bool(true)),
            ("renamed", Value::Int(3)),
        ])
    );
    // The skipped field is absent from the wire and comes back as `Default`,
    // not as the 99 that went in.
    assert_eq!(
        hostile::Shadowed::from_value(&lowered),
        Ok(hostile::Shadowed {
            cache: hostile::Cache(0),
            ..original
        })
    );
}

#[test]
fn a_struct_with_no_shared_fields_is_still_a_map_and_still_refuses_other_values() {
    let lowered = hostile::AllSkipped {
        cache: hostile::Cache(1),
    }
    .to_value();
    assert_eq!(lowered, Value::map([]));
    assert_eq!(
        hostile::AllSkipped::from_value(&lowered),
        Ok(hostile::AllSkipped {
            cache: hostile::Cache(0)
        })
    );
    assert!(hostile::AllSkipped::from_value(&Value::Int(1)).is_err());

    let schema = Schema::of::<hostile::AllSkipped>().expect("a schema with no fields is valid");
    assert!(
        schema.render().contains("\"fields\": []"),
        "{}",
        schema.render()
    );
}

#[test]
fn the_crate_attribute_names_a_module_that_is_not_the_duet_crate() {
    let through = hostile::ThroughAFacade {
        counter: 4,
        cache: hostile::Cache(0),
    };
    assert_eq!(through.to_value(), Value::map([("counter", Value::Int(4))]));
    assert!(Schema::of::<hostile::ThroughAFacade>().is_ok());
}

#[test]
fn a_type_derived_in_a_hostile_module_installs_and_reads_back_through_the_store() {
    // The whole path, not just the codec: seed a real store from the derived
    // schema, then read one field back through the typed handle a generated
    // Rust client would use.
    let runtime = Runtime::spawn(Value::Null, NullSink);
    let store = install(
        &runtime,
        &hostile::Shadowed {
            counter: 7,
            ratio: 1.5,
            flag: false,
            original: 3,
            cache: hostile::Cache(0),
        },
    );
    assert_eq!(
        store
            .field::<i64>("renamed")
            .expect("renamed is a path")
            .get(),
        Ok(Reading::Present(3))
    );
    runtime.shutdown().expect("the runtime should stop cleanly");
}

#[test]
fn the_generated_commands_work_where_every_prelude_name_is_shadowed() {
    // The expansion compiles in the hostile module — which is most of the
    // check, since a relative path in `generate.rs` would not get this far —
    // and then answers correctly over a real registry.
    use duet::{CommandEntry, Commands, Schema, commands, describe};
    use duet_protocol::handle_text_with;

    static COMMANDS: [CommandEntry; 3] = commands![
        hostile::shadowed_subtract,
        hostile::shadowed_nothing,
        hostile::shadowed_through_a_facade
    ];

    let runtime = Runtime::spawn(Value::Null, NullSink);
    let registry = Commands::from_entries(&COMMANDS);
    assert_eq!(
        handle_text_with(
            &runtime.handle(),
            duet::SubscriberId(1),
            &registry,
            r#"{"kind":"invoke","id":"1","command":"shadowed_subtract","args":{"t":"m","v":{"a":{"t":"i","v":"10"},"renamed":{"t":"i","v":"3"},"note":{"t":"n"}}}}"#,
        ),
        r#"{"id":"1","kind":"returned","value":{"t":"i","v":"7"}}"#
    );
    assert_eq!(
        handle_text_with(
            &runtime.handle(),
            duet::SubscriberId(1),
            &registry,
            r#"{"kind":"invoke","id":"2","command":"shadowed_nothing","args":{"t":"m","v":{}}}"#,
        ),
        r#"{"id":"2","kind":"returned","value":{"t":"n"}}"#
    );
    assert_eq!(
        handle_text_with(
            &runtime.handle(),
            duet::SubscriberId(1),
            &registry,
            r#"{"kind":"invoke","id":"3","command":"through.a.facade","args":{"t":"m","v":{"a":{"t":"i","v":"4"}}}}"#,
        ),
        r#"{"id":"3","kind":"returned","value":{"t":"i","v":"4"}}"#,
        "the command named through the re-export must be the one that ran"
    );

    let schema = Schema::of_with_commands::<i64>(|r| describe(&COMMANDS, r))
        .expect("the shadowed commands describe a valid schema");
    assert_eq!(schema.commands().len(), 3);
    runtime.shutdown().expect("the runtime should stop cleanly");
}

/// Installs `state` and hands back the typed store, so the test above reads as
/// one thought.
fn install<T: SharedState>(runtime: &Runtime, state: &T) -> duet::TypedStore {
    duet::install(runtime.handle(), state).expect("the derived state should install")
}
