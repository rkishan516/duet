//! Mutation testing: break one thing about the input at a time, and record
//! which check notices.
//!
//! # Why this file exists
//!
//! `tests/schema_proof.rs` passes. So would a version of it that compared the
//! derive's output against a file the derive had just written, and so would one
//! whose assertion could not reach a failure at all. This project has been
//! bitten ten times by exactly that. The only way to know a check works is to
//! have watched it fail on purpose.
//!
//! Each mutation below is a **plausible developer edit** to the Rust type — a
//! reordered field, a misspelled key, a widened number — and each is checked
//! against the same three artifacts the real pipeline uses. The
//! `every_check_passes_on_the_unmutated_type` test is what stops the table
//! being fiction: a check that fired on everything, including the correct type,
//! would score a perfect catch rate while measuring nothing.
//!
//! # The three checks
//!
//! | Check | What it compares | What it can see |
//! |---|---|---|
//! | `schema_bytes` | the rendered schema against `schema/app.json` | everything, and so localises nothing |
//! | `generated_clients` | `duet-codegen`'s output against the committed Dart golden | what a guest developer would compile against |
//! | `wire_shape` | `to_value` against the seed the schema installs | what a *second guest* would find in the store |
//!
//! The interesting row is field order: it changes the schema and it changes the
//! generated positional constructors, and it changes **nothing** about the wire,
//! because a `Value::Map` sorts its keys. That is measured here rather than
//! reasoned about, and it is why `TypeDef` documents field order as part of the
//! contract — nothing downstream of the wire would have caught it.

mod support;

use duet::{Schema, SharedState, Value};
use duet_codegen::{Options, TsImports};

/// The type `schema/app.json` describes, unmutated.
#[derive(SharedState)]
struct App {
    counter: i64,
    editor: Editor,
    title: String,
}

#[derive(SharedState)]
struct Editor {
    zoom: f64,
    theme: String,
}

/// A reordered declaration. The schema records declaration order, and generated
/// constructors take their positional arguments in it.
mod field_order {
    use super::{Editor, SharedState};

    #[derive(SharedState)]
    pub struct App {
        pub title: String,
        pub counter: i64,
        pub editor: Editor,
    }
}

/// A wire key with a typo in it. The kind of edit a `rename` invites.
mod key_spelling {
    use super::{Editor, SharedState};

    #[derive(SharedState)]
    pub struct App {
        pub counter: i64,
        pub editor: Editor,
        #[duet(rename = "titel")]
        pub title: String,
    }
}

/// A field widened from `i64` to `f64`. Compiles, runs, and means something
/// different on the wire.
mod type_mapping {
    use super::{Editor, SharedState};

    #[derive(SharedState)]
    pub struct App {
        pub counter: f64,
        pub editor: Editor,
        pub title: String,
    }
}

/// A key misspelled one level down, inside the nested struct.
mod nested_key {
    use super::SharedState;

    #[derive(SharedState)]
    pub struct App {
        pub counter: i64,
        pub editor: Editor,
        pub title: String,
    }

    #[derive(SharedState)]
    pub struct Editor {
        #[duet(rename = "Zoom")]
        pub zoom: f64,
        pub theme: String,
    }
}

/// One check, and whether it noticed.
struct Report {
    name: &'static str,
    caught: bool,
}

/// Runs all three checks over the schema and value of one type.
fn checks<T: SharedState>(zero: &T) -> Vec<Report> {
    let schema = Schema::of::<T>().expect("every type in this file has a valid schema");
    vec![
        Report {
            name: "schema_bytes",
            caught: schema.render() != support::read("schema/app.json"),
        },
        Report {
            name: "generated_clients",
            caught: dart(&schema) != support::read("packages/duet/test/generated/app.duet.dart"),
        },
        Report {
            name: "wire_shape",
            caught: shape(&zero.to_value()) != shape(&duet::schema::seed(&committed())),
        },
    ]
}

/// The committed specification, parsed.
fn committed() -> Schema {
    duet_codegen::read_schema(&support::read("schema/app.json"))
        .expect("schema/app.json is a valid schema")
}

/// The Dart client `duet-codegen` emits for `schema`.
///
/// The options restate the ones `crates/duet-codegen/tests/support/mod.rs`
/// generates the committed goldens with. They are a second copy, and the
/// unmutated-baseline test below is what keeps it honest: a wrong option would
/// change the header, every comparison would differ, and the check would report
/// a perfect catch rate while measuring nothing.
fn dart(schema: &Schema) -> String {
    let options = Options {
        source: "schema/app.json".to_string(),
        command: "cargo test -p duet-codegen --test goldens -- --ignored regenerate_goldens"
            .to_string(),
        ts_imports: TsImports {
            root: "../../src/index.ts".to_string(),
            typed: "../../src/typed/index.ts".to_string(),
        },
    };
    match duet_codegen::generate(schema, &options) {
        Ok(generated) => generated.dart,
        // An unemittable mutant is a difference too — and saying so here keeps
        // the check total rather than panicking on a case a future mutation
        // reaches.
        Err(refused) => format!("refused: {refused}"),
    }
}

/// A value's structure with its data removed: variant names and map keys only.
///
/// This is what a second guest can observe about a node without agreeing on its
/// contents, and it is deliberately blind to ordering inside a map — because
/// `Value::Map` is a `BTreeMap` and sorts its keys, so nothing on the wire
/// carries declaration order at all.
fn shape(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(_) => "bool".to_string(),
        Value::Int(_) => "int".to_string(),
        Value::Float(_) => "float".to_string(),
        Value::Str(_) => "string".to_string(),
        Value::Bytes(_) => "bytes".to_string(),
        Value::List(items) => format!(
            "[{}]",
            items.iter().map(shape).collect::<Vec<_>>().join(",")
        ),
        Value::Map(entries) => format!(
            "{{{}}}",
            entries
                .iter()
                .map(|(key, value)| format!("{key}:{}", shape(value)))
                .collect::<Vec<_>>()
                .join(",")
        ),
    }
}

/// The names of the checks that noticed.
fn caught_by<T: SharedState>(zero: &T) -> Vec<&'static str> {
    checks(zero)
        .into_iter()
        .filter(|report| report.caught)
        .map(|report| report.name)
        .collect()
}

fn zero_editor() -> Editor {
    Editor {
        zoom: 0.0,
        theme: String::new(),
    }
}

#[test]
fn every_check_passes_on_the_unmutated_type() {
    // The measurement that makes the table below mean anything. Without it a
    // check that always fires would look like the most sensitive one here.
    let zero = App {
        counter: 0,
        editor: zero_editor(),
        title: String::new(),
    };
    assert_eq!(caught_by(&zero), Vec::<&str>::new());
}

#[test]
fn a_reordered_field_is_caught_by_the_schema_and_the_clients_but_not_by_the_wire() {
    let zero = field_order::App {
        title: String::new(),
        counter: 0,
        editor: zero_editor(),
    };
    assert_eq!(caught_by(&zero), ["schema_bytes", "generated_clients"]);
}

#[test]
fn a_misspelled_key_is_caught_by_all_three() {
    let zero = key_spelling::App {
        counter: 0,
        editor: zero_editor(),
        title: String::new(),
    };
    assert_eq!(
        caught_by(&zero),
        ["schema_bytes", "generated_clients", "wire_shape"]
    );
}

#[test]
fn a_widened_number_is_caught_by_all_three() {
    let zero = type_mapping::App {
        counter: 0.0,
        editor: zero_editor(),
        title: String::new(),
    };
    assert_eq!(
        caught_by(&zero),
        ["schema_bytes", "generated_clients", "wire_shape"]
    );
}

#[test]
fn a_key_misspelled_one_level_down_is_caught_by_all_three() {
    let zero = nested_key::App {
        counter: 0,
        editor: nested_key::Editor {
            zoom: 0.0,
            theme: String::new(),
        },
        title: String::new(),
    };
    assert_eq!(
        caught_by(&zero),
        ["schema_bytes", "generated_clients", "wire_shape"]
    );
}
