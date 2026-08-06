//! Generates and verifies `corpus/wire-corpus.json`, the cross-language golden
//! wire corpus.
//!
//! Three implementations of one wire format — Rust here, a Dart guest client,
//! and a JavaScript guest client — drift apart silently, because each one's
//! tests are written against its own encoder. Four such divergences have
//! already been found and fixed in this project: non-canonical ids causing a
//! silent hang, `-0.0` losing its sign in JavaScript, an id domain wider in
//! Rust than Dart can represent, and map key order differing between UTF-8
//! bytes and UTF-16 code units. Every one of them was invisible to a
//! single-language round-trip test.
//!
//! The corpus is the artifact that stops the next one. Rust generates it; Dart
//! and TypeScript consume the same file.
//!
//! # The three tests
//!
//! - [`corpus_matches_the_committed_file`] runs in CI: regenerate in memory,
//!   compare bytes. It fails if the codec changed without the corpus being
//!   regenerated and reviewed.
//! - [`rust_satisfies_its_own_corpus`] runs the guests' own checks against the
//!   committed file. If Rust cannot pass what Rust produced, the corpus is
//!   wrong and every guest would be chasing a phantom.
//! - `regenerate_corpus` is `#[ignore]`d and writes the file. Run by hand.

mod corpus;

use std::fs;

use serde::Deserialize;

/// Reads the committed corpus file as JSON, with `serde_json`'s recursion limit
/// lifted **for this file only**.
///
/// # Why the fixture container is exempt from the limit it describes
///
/// `value/nesting/at_limit` is a message at exactly
/// [`duet_codec::MAX_JSON_DEPTH`] containers — the deepest thing the wire
/// admits. Its `wire` is stored as a JSON *string*, so it costs the file
/// nothing; its `witness` is stored as a JSON *tree*, and a witness is the same
/// depth as the value it witnesses. Sitting three containers below the document
/// root (`{…} → "accept" → case → witness`), that puts the file two containers
/// past the limit.
///
/// So the corpus can only state its own boundary case if the envelope carrying
/// it is exempt from that boundary. Nothing is weakened by this: the limit
/// applies to guest text arriving over IPC, and `duet_protocol::handle_text`
/// enforces it there with `duet_codec::exceeds_max_json_depth` before any parse.
/// This file is neither guest text nor untrusted — it is a build artifact of
/// this repository, read by a test.
///
/// The two guest implementations need no equivalent: `jsonDecode` and
/// `JSON.parse` have no depth limit of their own, which is precisely why both
/// packages have to state the wire's limit explicitly instead.
fn read_corpus(text: &str) -> serde_json::Value {
    let mut de = serde_json::Deserializer::from_str(text);
    de.disable_recursion_limit();
    serde_json::Value::deserialize(&mut de)
        .unwrap_or_else(|e| panic!("the corpus is not valid JSON: {e}"))
}

#[test]
fn corpus_matches_the_committed_file() {
    let path = corpus::corpus_path();
    let committed = fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "cannot read {}: {e}\n\nRegenerate it with:\n    {}",
            path.display(),
            corpus::GENERATOR
        )
    });
    let generated = corpus::render();
    if committed != generated {
        panic!(
            "{} is out of date{}\n\nRegenerate it with:\n    {}",
            path.display(),
            first_difference(&committed, &generated),
            corpus::GENERATOR
        );
    }
}

/// Names the first differing line, so a failure points at the change rather
/// than dumping two copies of a multi-thousand-line file.
fn first_difference(committed: &str, generated: &str) -> String {
    for (n, (a, b)) in committed.lines().zip(generated.lines()).enumerate() {
        if a != b {
            return format!(
                " — first difference at line {}:\n  committed: {a}\n  generated: {b}",
                n + 1
            );
        }
    }
    format!(
        " — identical for {} lines, then the length differs ({} committed vs {} generated)",
        committed.lines().count().min(generated.lines().count()),
        committed.lines().count(),
        generated.lines().count()
    )
}

#[test]
fn rust_satisfies_its_own_corpus() {
    let path = corpus::corpus_path();
    let committed =
        fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    corpus::check::verify(&read_corpus(&committed));
}

#[test]
fn the_corpus_covers_every_reason_and_every_layer() {
    // A corpus that silently lost half its cases would still pass the two
    // tests above — they check what is there, not what is missing. These
    // are the coverage floors: every rejection reason the wire can produce,
    // and every decoder a guest implements.
    let reject = corpus::cases::reject_cases();
    for reason in [
        "unknown_tag",
        "bad_shape",
        "bad_int",
        "bad_float",
        "bad_base64",
        "bad_path",
        corpus::BAD_JSON,
    ] {
        assert!(
            reject.iter().any(|c| c.reason == reason),
            "no reject case exercises {reason:?}"
        );
    }

    let accept = corpus::cases::accept_cases();
    for layer in [
        corpus::Layer::Value,
        corpus::Layer::Request,
        corpus::Layer::Response,
        corpus::Layer::Push,
    ] {
        assert!(
            accept.iter().any(|c| c.layer == layer),
            "no accept case exercises the {:?} layer",
            layer
        );
    }

    // Both polarities of `reencode_byte_exact` must appear, or the flag is
    // untested in every guest that reads it.
    assert!(accept.iter().any(|c| c.reencode_byte_exact));
    assert!(accept.iter().any(|c| !c.reencode_byte_exact));
    // And at least one case must exercise `reencodes_to`, which pins the
    // places the decoder is deliberately wider than the encoder.
    assert!(accept.iter().any(|c| c.reencodes_to.is_some()));
}

#[test]
#[ignore = "writes corpus/wire-corpus.json; run by hand, then review the diff"]
fn regenerate_corpus() {
    let path = corpus::corpus_path();
    let dir = path
        .parent()
        .unwrap_or_else(|| panic!("{} should have a parent", path.display()));
    fs::create_dir_all(dir).unwrap_or_else(|e| panic!("cannot create {}: {e}", dir.display()));
    fs::write(&path, corpus::render())
        .unwrap_or_else(|e| panic!("cannot write {}: {e}", path.display()));
}
