//! Verifying a corpus document against this workspace's own codec.
//!
//! If Rust cannot pass the corpus Rust generated, the corpus is wrong and every
//! guest implementing against it would be chasing a phantom. So this runs the
//! same checks a Dart or TypeScript guest will run, against the *committed*
//! file rather than a freshly built document — which also catches a file that
//! was hand-edited after generation.

use serde_json::Value as Json;

use super::{BAD_JSON, Layer, contains_json_number, decode_and_reencode, parse};

/// Reads a required string field from a case object.
fn text(case: &Json, field: &str) -> String {
    case.get(field)
        .and_then(Json::as_str)
        .unwrap_or_else(|| panic!("corpus case is missing a string {field:?}: {case}"))
        .to_string()
}

/// Reads a required array field from the document.
fn array<'a>(document: &'a Json, field: &str) -> &'a Vec<Json> {
    document
        .get(field)
        .and_then(Json::as_array)
        .unwrap_or_else(|| panic!("corpus is missing the {field:?} array"))
}

fn layer_of(case: &Json) -> Layer {
    match text(case, "layer").as_str() {
        "value" => Layer::Value,
        "request" => Layer::Request,
        "response" => Layer::Response,
        "push" => Layer::Push,
        other => panic!("unknown corpus layer {other:?} in {case}"),
    }
}

/// Checks one accept case: it decodes, it decodes to the recorded witness, and
/// re-encoding reproduces the recorded text byte for byte.
fn check_accept(case: &Json) {
    let name = text(case, "name");
    let wire = text(case, "wire");
    let layer = layer_of(case);
    let json = serde_json::from_str(&wire)
        .unwrap_or_else(|e| panic!("{name}: wire must be valid JSON: {e}"));

    let (witness, canonical) = decode_and_reencode(layer, &json)
        .unwrap_or_else(|e| panic!("{name}: must decode, got {} ({e})", e.reason_code()));

    let expected = case
        .get("witness")
        .unwrap_or_else(|| panic!("{name}: missing witness"));
    assert_eq!(&witness, expected, "{name}: decoded to the wrong value");

    // `reencodes_to: null` means "re-encodes to `wire`".
    let target = match case.get("reencodes_to") {
        Some(Json::Null) | None => wire,
        Some(_) => text(case, "reencodes_to"),
    };
    assert_eq!(canonical, target, "{name}: re-encoded to the wrong text");

    let exact = case
        .get("reencode_byte_exact")
        .and_then(Json::as_bool)
        .unwrap_or_else(|| panic!("{name}: missing reencode_byte_exact"));
    assert_eq!(
        exact,
        !contains_json_number(&parse(&canonical)),
        "{name}: reencode_byte_exact disagrees with the encoding it describes"
    );
}

/// Checks one reject case: it fails, and it fails for the recorded reason.
///
/// The reason is the point. Asserting only "is an error" would let a codec
/// mutated anywhere still pass every reject case, refusing each one for a new
/// and wrong reason.
fn check_reject(case: &Json) {
    let name = text(case, "name");
    let wire = text(case, "wire");
    let reason = text(case, "reason");

    let json: Json = match serde_json::from_str(&wire) {
        Ok(json) => json,
        Err(e) => {
            assert_eq!(
                reason, BAD_JSON,
                "{name}: JSON parsing failed ({e}), so the reason must be {BAD_JSON}"
            );
            return;
        }
    };
    assert_ne!(
        reason, BAD_JSON,
        "{name}: reason is {BAD_JSON} but the text parsed as JSON just fine"
    );

    match decode_and_reencode(layer_of(case), &json) {
        Ok((witness, _)) => panic!("{name}: must be rejected, but decoded to {witness}"),
        Err(e) => assert_eq!(
            e.reason_code(),
            reason,
            "{name}: rejected for the wrong reason ({e})"
        ),
    }
}

/// Case names must be unique: a failure has to name itself unambiguously.
fn check_names_unique(document: &Json) {
    let mut seen = std::collections::BTreeSet::new();
    for case in array(document, "accept")
        .iter()
        .chain(array(document, "reject"))
    {
        let name = text(case, "name");
        assert!(
            name.contains('/'),
            "corpus case name {name:?} must be hierarchical"
        );
        assert!(
            seen.insert(name.clone()),
            "duplicate corpus case name {name:?}"
        );
    }
}

/// Runs every check against a corpus document.
pub fn verify(document: &Json) {
    assert_eq!(
        document.get("version"),
        Some(&Json::from(super::VERSION)),
        "unexpected corpus schema version"
    );
    check_names_unique(document);
    for case in array(document, "accept") {
        check_accept(case);
    }
    for case in array(document, "reject") {
        check_reject(case);
    }
}
