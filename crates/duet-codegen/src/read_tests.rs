//! Unit tests for the reader's edges. The rejection *messages* are pinned in
//! `tests/negative.rs`, against committed fixtures; these reach the boundaries a
//! fixture would be an awkward way to express.

use super::*;
use duet_schema::SCHEMA_VERSION;

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

/// A document at `version` with `extra` spliced in before it.
///
/// `extra` is written as complete `"key": value,` text so a test can add a key
/// the reader may not know, which is the whole point of the version tolerance.
fn document(extra: &str, version: u32) -> String {
    format!(
        "{{{extra}\"root\": {{\"kind\": \"named\", \"name\": \"App\"}}, \"types\": \
         [{{\"fields\": [], \"name\": \"App\"}}], \"version\": {version}}}"
    )
}

#[test]
fn the_reader_accepts_the_version_the_writer_emits() {
    // The reader must accept at least what the writer produces. Pinned because
    // a bumped `SCHEMA_VERSION` with an unbumped reader would refuse every
    // schema the workspace produces, and the failure would surface at the far
    // end of a pipeline rather than here.
    let text = with_field("{\"kind\": \"int\"}");
    assert!(read_schema(&text).is_ok());
    assert!(text.contains(&format!("\"version\": {SCHEMA_VERSION}")));
    assert!(
        SUPPORTED_VERSIONS.contains(&SCHEMA_VERSION),
        "the writer emits {SCHEMA_VERSION}, which the reader does not accept"
    );
}

#[test]
fn every_supported_version_is_accepted() {
    // The tolerance itself, both directions at once: a newer reader meeting an
    // older file, and an older reader meeting a newer one.
    for version in SUPPORTED_VERSIONS {
        assert!(
            read_schema(&document("", *version)).is_ok(),
            "version {version} is listed as supported but is refused"
        );
    }
}

#[test]
fn a_version_outside_the_supported_set_is_refused_and_the_message_names_the_set() {
    // The bound on either side, so a set that quietly grew a member is visible.
    for version in [0, 3, 99] {
        let error = read_schema(&document("", version)).expect_err("unsupported");
        assert!(
            matches!(error, ReadError::UnsupportedVersion { .. }),
            "version {version}: {error}"
        );
        let message = error.to_string();
        assert!(message.contains("1 and 2"), "version {version}: {message}");
    }
}

#[test]
fn a_version_2_document_may_carry_commands_and_this_reader_ignores_them() {
    // The forward-tolerance path, tested here rather than discovered later.
    // This reader has nowhere to put a command definition, so it reads the
    // state half of the document and leaves the rest alone.
    let commands = "\"commands\": [{\"name\": \"add\", \"params\": \
                    [{\"key\": \"a\", \"type\": {\"kind\": \"int\"}}], \
                    \"returns\": {\"kind\": \"int\"}}], ";
    let schema = read_schema(&document(commands, 2)).expect("a version-2 document is readable");
    assert_eq!(schema.root(), &Ty::Named("App".to_string()));
    assert_eq!(schema.types().len(), 1);
}

#[test]
fn a_version_2_document_without_commands_is_readable() {
    // `commands` is optional at version 2, so a schema that declares none is
    // not a special case the reader has to be told about.
    assert!(read_schema(&document("", 2)).is_ok());
}

#[test]
fn commands_in_a_version_1_document_are_refused() {
    // The key set is a function of the version. A version-1 document carrying
    // `commands` is a file that means one thing to a reader that knows the key
    // and another to one that does not, which is worse than a file nobody
    // accepts.
    let error = read_schema(&document("\"commands\": [], ", 1)).expect_err("not a version-1 key");
    assert!(
        matches!(&error, ReadError::UnexpectedKey { key, .. } if key == "commands"),
        "{error}"
    );
}

#[test]
fn a_commands_key_that_is_not_an_array_is_refused_even_though_it_is_ignored() {
    // "Ignore the content" and "accept any bytes at all" are different
    // promises. A corrupt `commands` is a corrupt document however little of it
    // this reader consumes.
    let error = read_schema(&document("\"commands\": 7, ", 2)).expect_err("not an array");
    assert!(
        matches!(&error, ReadError::WrongKind { at, .. } if at == "\"commands\""),
        "{error}"
    );
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
