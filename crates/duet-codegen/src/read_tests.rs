//! Unit tests for the reader's edges. The rejection *messages* are pinned in
//! `tests/negative.rs`, against committed fixtures; these reach the boundaries a
//! fixture would be an awkward way to express.

use super::*;

/// A document with one field of the given type.
fn with_field(ty: &str) -> String {
    format!(
        "{{\"root\": {{\"kind\": \"named\", \"name\": \"App\"}}, \"types\": \
         [{{\"fields\": [{{\"key\": \"f\", \"type\": {ty}}}], \"name\": \"App\"}}], \
         \"version\": 1}}"
    )
}

/// `n` nested `kind` constructors around an `int`.
fn nested(kind: &str, n: usize) -> String {
    let mut ty = "{\"kind\": \"int\"}".to_string();
    for _ in 0..n {
        ty = format!("{{\"kind\": \"{kind}\", \"of\": {ty}}}");
    }
    ty
}

#[test]
fn a_type_nested_exactly_to_the_limit_is_accepted() {
    // The boundary from below. `optional` rather than `list` because an option
    // adds no container to the *store's* depth, so this reaches the reader's own
    // limit rather than `Schema::build`'s.
    let text = with_field(&nested("optional", MAX_TY_DEPTH));
    assert!(read_schema(&text).is_ok(), "{MAX_TY_DEPTH} should be legal");
}

#[test]
fn one_constructor_past_the_limit_is_refused() {
    let text = with_field(&nested("optional", MAX_TY_DEPTH + 1));
    let error = read_schema(&text).expect_err("past the limit");
    assert!(matches!(error, ReadError::TooDeep { .. }), "{error}");
    assert!(error.to_string().contains("types[0].fields[0].type"));
}

#[test]
fn the_error_names_where_in_the_document_it_looked() {
    // A schema file is edited by hand. "something is wrong" is not a message
    // anyone can act on; the trail from the document root is.
    let text = with_field("{\"kind\": \"list\", \"of\": {\"kind\": \"nope\"}}");
    let message = read_schema(&text).expect_err("unknown kind").to_string();
    assert!(
        message.contains("types[0].fields[0].type.of"),
        "no location in: {message}"
    );
    assert!(message.contains("nope"), "no kind in: {message}");
}

#[test]
fn the_reader_and_the_writer_agree_on_the_version_they_speak() {
    // The reader accepts exactly what the writer emits. Pinned because a bumped
    // `SCHEMA_VERSION` with an unbumped reader would refuse every schema the
    // workspace produces, and the failure would be at the far end of a pipeline.
    let text = with_field("{\"kind\": \"int\"}");
    assert!(read_schema(&text).is_ok());
    assert!(text.contains(&format!("\"version\": {SCHEMA_VERSION}")));
}

#[test]
fn an_empty_types_array_is_legal_when_nothing_refers_to_one() {
    let schema = read_schema("{\"root\": {\"kind\": \"int\"}, \"types\": [], \"version\": 1}")
        .expect("a scalar root needs no types");
    assert_eq!(schema.root(), &Ty::Int);
    assert!(schema.types().is_empty());
}

#[test]
fn a_type_with_no_fields_is_legal() {
    // An empty struct is a `Value::Map` with no entries. Unusual, not wrong,
    // and the emitters must not divide by its field count.
    let schema = read_schema(
        "{\"root\": {\"kind\": \"named\", \"name\": \"Empty\"}, \"types\": \
         [{\"fields\": [], \"name\": \"Empty\"}], \"version\": 1}",
    )
    .expect("an empty struct is legal");
    assert!(schema.types()[0].fields.is_empty());
}

#[test]
fn every_scalar_kind_reads_back_as_its_arm() {
    for (kind, arm) in [
        ("bool", Ty::Bool),
        ("int", Ty::Int),
        ("float", Ty::Float),
        ("string", Ty::Str),
        ("bytes", Ty::Bytes),
        ("dynamic", Ty::Dynamic),
    ] {
        let schema = read_schema(&with_field(&format!("{{\"kind\": \"{kind}\"}}")))
            .unwrap_or_else(|e| panic!("{kind} should read: {e}"));
        assert_eq!(schema.types()[0].fields[0].ty, arm, "{kind}");
    }
}

#[test]
fn every_wrapper_kind_reads_back_around_its_inner_type() {
    for (kind, wrap) in [
        ("optional", Ty::optional as fn(Ty) -> Ty),
        ("list", Ty::list),
        ("map", Ty::map),
    ] {
        let schema = read_schema(&with_field(&nested(kind, 1)))
            .unwrap_or_else(|e| panic!("{kind} should read: {e}"));
        assert_eq!(schema.types()[0].fields[0].ty, wrap(Ty::Int), "{kind}");
    }
}

#[test]
fn a_named_reference_carries_the_name_verbatim() {
    let schema = read_schema(
        "{\"root\": {\"kind\": \"named\", \"name\": \"App\"}, \"types\": \
         [{\"fields\": [{\"key\": \"e\", \"type\": {\"kind\": \"named\", \"name\": \"Editor\"}}], \
         \"name\": \"App\"}, {\"fields\": [], \"name\": \"Editor\"}], \"version\": 1}",
    )
    .expect("a legal schema");
    assert_eq!(
        schema.types()[0].fields[0].ty,
        Ty::Named("Editor".to_string())
    );
}

#[test]
fn field_order_is_the_document_order_not_the_alphabet() {
    // Declaration order is part of the contract; a reader that sorted would
    // silently reorder every generated constructor.
    let schema = read_schema(
        "{\"root\": {\"kind\": \"named\", \"name\": \"App\"}, \"types\": \
         [{\"fields\": [{\"key\": \"zebra\", \"type\": {\"kind\": \"int\"}}, \
         {\"key\": \"apple\", \"type\": {\"kind\": \"int\"}}], \"name\": \"App\"}], \"version\": 1}",
    )
    .expect("a legal schema");
    let keys: Vec<&str> = schema.types()[0]
        .fields
        .iter()
        .map(|f| f.key.as_str())
        .collect();
    assert_eq!(keys, ["zebra", "apple"]);
}
