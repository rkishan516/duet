//! What the three generated methods do, including on input no host wrote.
//!
//! `from_value` is a decoder for hostile input, not a deserializer for data the
//! host produced: a second guest can write any value to any path while the first
//! is reading it. Everything below that hands a wrong-shaped `Value` to a
//! derived `from_value` is checking that promise, and that the answer is a
//! located error rather than a panic on the thread that owns the store.

use std::collections::BTreeMap;

use duet::{Bytes, DecodeError, Schema, SharedState, Value};

#[derive(Debug, Clone, PartialEq, SharedState)]
struct Document {
    title: String,
    editor: Editor,
    revisions: Vec<i64>,
    tags: BTreeMap<String, String>,
    thumbnail: Bytes,
    subtitle: Option<String>,
    #[duet(rename = "window_title")]
    window: String,
    #[duet(skip)]
    render_cache: Cache,
}

#[derive(Debug, Clone, PartialEq, SharedState)]
struct Editor {
    zoom: f64,
    theme: String,
}

/// Not shared state: an expensive thing the application rebuilds.
#[derive(Debug, Clone, Default, PartialEq)]
struct Cache {
    generation: i64,
}

fn sample() -> Document {
    Document {
        title: "draft".to_string(),
        editor: Editor {
            zoom: 1.5,
            theme: "dark".to_string(),
        },
        revisions: vec![1, 2, 3],
        tags: BTreeMap::from([("a".to_string(), "b".to_string())]),
        thumbnail: Bytes(vec![0xDE, 0xAD]),
        subtitle: None,
        window: "Duet".to_string(),
        render_cache: Cache { generation: 0 },
    }
}

#[test]
fn a_value_survives_a_round_trip_through_the_store_representation() {
    let original = sample();
    assert_eq!(Document::from_value(&original.to_value()), Ok(original));
}

#[test]
fn every_shared_field_is_materialized_even_when_it_is_none() {
    // `Option::None` is `Value::Null`, never an absent key: the store has no
    // `remove`, so an absent key is a schema violation rather than a
    // representable value, and a decoder that tolerated one would disagree with
    // the Dart and TypeScript clients.
    let Value::Map(entries) = sample().to_value() else {
        panic!("a struct lowers to a map")
    };
    assert_eq!(
        entries.keys().map(String::as_str).collect::<Vec<_>>(),
        [
            "editor",
            "revisions",
            "subtitle",
            "tags",
            "thumbnail",
            "title",
            "window_title"
        ]
    );
    assert_eq!(entries.get("subtitle"), Some(&Value::Null));
}

#[test]
fn a_skipped_field_reaches_neither_the_wire_nor_the_schema() {
    let original = Document {
        render_cache: Cache { generation: 41 },
        ..sample()
    };
    let lowered = original.to_value();
    let Value::Map(entries) = &lowered else {
        panic!("a struct lowers to a map")
    };
    assert!(!entries.contains_key("render_cache"));

    // And it comes back as `Default`, not as the 41 that went in — which is the
    // whole meaning of "not shared state".
    assert_eq!(
        Document::from_value(&lowered).map(|read| read.render_cache),
        Ok(Cache { generation: 0 })
    );

    let rendered = Schema::of::<Document>().expect("a schema").render();
    assert!(!rendered.contains("render_cache"), "{rendered}");
}

#[test]
fn a_renamed_field_is_addressed_by_its_key_and_never_by_its_rust_name() {
    let rendered = Schema::of::<Document>().expect("a schema").render();
    assert!(rendered.contains("\"key\": \"window_title\""), "{rendered}");
    assert!(!rendered.contains("\"window\""), "{rendered}");
}

/// The error a `Value` produced, as its rendered message.
fn refusal(value: &Value) -> String {
    Document::from_value(value)
        .expect_err("this value is not a Document")
        .to_string()
}

#[test]
fn a_value_that_is_not_a_map_is_refused_by_the_struct_name() {
    for value in [
        Value::Null,
        Value::Int(1),
        Value::Str("nope".into()),
        Value::Bytes(vec![1]),
        Value::List(Vec::new()),
    ] {
        assert_eq!(
            refusal(&value),
            format!(
                "expected Document at <root>, found {}",
                match value {
                    Value::Null => "null",
                    Value::Int(_) => "int",
                    Value::Str(_) => "string",
                    Value::Bytes(_) => "bytes",
                    _ => "list",
                }
            )
        );
    }
}

#[test]
fn an_absent_key_names_the_key_rather_than_defaulting_it() {
    let Value::Map(mut entries) = sample().to_value() else {
        panic!("a struct lowers to a map")
    };
    entries.remove("title");
    assert_eq!(
        refusal(&Value::Map(entries)),
        "Document at <root> is missing the key \"title\""
    );
}

#[test]
fn a_field_of_the_wrong_type_is_located_at_its_key() {
    let Value::Map(mut entries) = sample().to_value() else {
        panic!("a struct lowers to a map")
    };
    entries.insert("title".to_string(), Value::Int(1));
    assert_eq!(
        refusal(&Value::Map(entries)),
        "expected String at title, found int"
    );
}

#[test]
fn a_failure_inside_a_nested_struct_reports_the_whole_path() {
    // The location builds up outward through each derived `from_value` in turn,
    // which is what makes a mismatch reported by a Rust host point at the same
    // place a Dart or TypeScript client would name.
    let Value::Map(mut entries) = sample().to_value() else {
        panic!("a struct lowers to a map")
    };
    entries.insert(
        "editor".to_string(),
        Value::map([
            ("zoom", Value::Str("big".into())),
            ("theme", Value::Str("dark".into())),
        ]),
    );
    assert_eq!(
        refusal(&Value::Map(entries)),
        "expected f64 at editor.zoom, found string"
    );
}

#[test]
fn a_failure_inside_a_list_keeps_its_index() {
    let Value::Map(mut entries) = sample().to_value() else {
        panic!("a struct lowers to a map")
    };
    entries.insert(
        "revisions".to_string(),
        Value::List(vec![Value::Int(1), Value::Bool(true)]),
    );
    assert_eq!(
        refusal(&Value::Map(entries)),
        "expected i64 at revisions[1], found bool"
    );
}

#[test]
fn a_guest_may_write_anything_anywhere_and_the_decode_still_answers() {
    // Total over `Value`, as the trait requires. Nothing here asserts *which*
    // answer: the point is that every one of these returns rather than panics
    // on the thread that owns the store.
    let shapes = [
        Value::Null,
        Value::Bool(true),
        Value::Int(i64::MIN),
        Value::Float(f64::NAN),
        Value::Str(String::new()),
        Value::Bytes(Vec::new()),
        Value::List(vec![Value::Null; 8]),
        Value::map([("title", Value::map([("nested", Value::Null)]))]),
        Value::map([("editor", Value::List(Vec::new()))]),
    ];
    for value in &shapes {
        let _: Result<Document, DecodeError> = Document::from_value(value);
        let _: Result<Editor, DecodeError> = Editor::from_value(value);
    }
}

#[test]
fn the_derived_type_is_not_nullable_so_it_fits_inside_an_option() {
    // `impl NotNullable` is emitted unconditionally, and it is sound because a
    // struct lowers to `Value::Map` and never to `Value::Null`. Without it,
    // `Option<Editor>` would not compile — which `schema/wide.json` needs.
    assert_eq!(Option::<Editor>::None.to_value(), Value::Null);
    assert_eq!(
        Option::<Editor>::from_value(&Value::Null),
        Ok(Option::<Editor>::None)
    );
    assert!(matches!(
        Some(Editor {
            zoom: 1.0,
            theme: "dark".to_string()
        })
        .to_value(),
        Value::Map(_)
    ));
}
