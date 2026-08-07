//! Every command the committed clients call, resolved against a **live host**.
//!
//! # The check a golden cannot make
//!
//! `crates/duet-codegen`'s goldens prove the emitter still emits what it emitted
//! before. Nothing in a byte comparison can notice that
//! `client.invoke('documentsClose')` addresses **nothing** — a camel-cased
//! command name is not a syntax error, not a type error, and not a decode
//! error. It is a `failed` reply at the far end of a call the developer believed
//! was typed, and it would arrive only when someone ran the app.
//!
//! So this takes its input from the *artifact*: it scans the committed `.dart`
//! and `.ts` clients for the command names they invoke, and puts each one on the
//! wire against a real [`Session`] — a real [`duet_runtime::Runtime`], a real
//! store seeded from the schema, and the real hand-written registry. A name that
//! does not resolve fails here.
//!
//! It runs from this crate rather than from `duet-codegen` because the registry
//! lives here and `duet-codegen` cannot depend on the host it seeds.
//!
//! # And the schema is checked against the registry in both directions
//!
//! A guest's typed method exists because the *schema* declares a command; it
//! works because the *registry* answers one. Nothing in Rust ties the two
//! together, so a command declared and never registered is a method that cannot
//! be called, and a command registered and never declared is host surface no
//! generated client can reach. Both are failures here.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use duet_host_stdio::Session;

/// The fixture whose schema declares commands, and the two clients generated
/// from it.
const FIXTURE: &str = "app";

/// The committed clients this scans, relative to the repository root.
const CLIENTS: &[&str] = &[
    "packages/duet/test/generated/app.duet.dart",
    "packages/duet-js/test/generated/app.duet.ts",
    "examples/generated/app.duet.dart",
    "examples/generated/app.duet.ts",
];

/// The repository root, found from this crate rather than from the working
/// directory, which `cargo test` does not promise.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

/// Reads a file under the repository root.
fn read(relative: &str) -> String {
    let path = repo_root().join(relative);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

/// Every command name a generated client invokes, and the argument keys it
/// sends with it.
///
/// Deliberately dumb: both emitters spell an invocation as
/// `invoke('<name>'` and each argument as `'<key>':` or `['<key>',`, so one
/// scanner serves both languages. A smarter parser could share a bug with the
/// emitter it is checking.
fn invocations(text: &str) -> Vec<(String, Vec<String>)> {
    let mut found = Vec::new();
    for after in text.split("invoke('").skip(1) {
        let Some(end) = after.find('\'') else {
            continue;
        };
        let name = after[..end].to_string();
        found.push((name, argument_keys(&after[end + 1..])));
    }
    found
}

/// The argument keys of one invocation, read from the lines after its name.
///
/// Both emitters put exactly one argument on each line, and both open it with
/// the key as a single-quoted literal: `'by': …` in Dart and `['by', …]` in
/// TypeScript. So the scan is line-oriented and stops at the first line that is
/// neither — which for a command with no arguments is the very first one.
///
/// Line-oriented rather than "everything up to the closing bracket", because a
/// generated argument value is a codec call and *contains* brackets:
/// `duetIntCodec.encode(by),` ends in `),` and a scanner that stopped there
/// would find the first key and silently lose the rest.
fn argument_keys(rest: &str) -> Vec<String> {
    let mut keys = Vec::new();
    for line in rest.lines().skip(1) {
        let trimmed = line.trim_start();
        let opened = trimmed.strip_prefix("['").or_else(|| {
            trimmed
                .strip_prefix('\'')
                .filter(|_| trimmed.contains("':"))
        });
        let Some(opened) = opened else { break };
        let Some(end) = opened.find('\'') else { break };
        keys.push(opened[..end].to_string());
    }
    keys
}

/// The `failed` reply a host sends for a command it does not have.
///
/// Matched on the registry's own words rather than on the whole message, so a
/// reworded refusal does not silently turn this test into one that passes for
/// every reply.
const UNREGISTERED: &str = "is registered for this surface";

#[test]
fn every_command_the_committed_clients_invoke_resolves_on_a_live_host() {
    let session = Session::open(FIXTURE).expect("the app fixture should open");
    let mut checked = 0usize;
    for client in CLIENTS {
        let text = read(client);
        let found = invocations(&text);
        assert!(
            !found.is_empty(),
            "{client}: the scanner found no invocations, so this test proves nothing"
        );
        for (name, _) in &found {
            let request = format!(
                r#"{{"kind":"invoke","id":"1","command":{},"args":{{"t":"m","v":{{}}}}}}"#,
                serde_json::to_string(name).expect("a string always serializes")
            );
            let reply = duet_protocol::handle_text_with(
                &session.handle(),
                duet_core::SubscriberId(1),
                session.commands(),
                &request,
            );
            assert!(
                !reply.contains(UNREGISTERED),
                "{client}: the generated client invokes {name:?}, which this host \
                 does not register — the call would fail at run time with no \
                 error before it. Reply: {reply}"
            );
            checked += 1;
        }
    }
    assert!(checked >= 8, "only {checked} invocations were checked");
    session.shutdown().expect("shutdown should succeed");
}

#[test]
fn a_camel_cased_command_name_would_not_resolve() {
    // The negative half: the check above only means something if a camel-cased
    // name genuinely fails against the host. `session.ping` exists and
    // `sessionPing` must not — which is precisely the silent failure the naming
    // rule exists to prevent, and precisely what a golden test cannot see.
    let session = Session::open(FIXTURE).expect("the app fixture should open");
    let ask = |command: &str| {
        duet_protocol::handle_text_with(
            &session.handle(),
            duet_core::SubscriberId(1),
            session.commands(),
            &format!(
                r#"{{"kind":"invoke","id":"1","command":"{command}","args":{{"t":"m","v":{{}}}}}}"#
            ),
        )
    };
    assert!(
        !ask(duet_host_stdio::PING).contains(UNREGISTERED),
        "the wire name must resolve"
    );
    assert!(
        ask("sessionPing").contains(UNREGISTERED),
        "the camel-cased name must not resolve"
    );
    session.shutdown().expect("shutdown should succeed");
}

#[test]
fn every_argument_key_a_generated_client_sends_is_one_the_schema_declares() {
    // The keys travel in a map the host decodes **by name**, so a camel-cased
    // or misspelled key is a `failed` naming a missing argument — again with
    // nothing before run time to notice. Checked against the schema this host
    // is seeded from, which is the same document the clients were generated
    // from and the thing both sides must agree with.
    let session = Session::open(FIXTURE).expect("the app fixture should open");
    let schema = duet_codegen::read_schema(session.fixture().text).expect("the fixture parses");
    let mut checked = 0usize;
    for client in CLIENTS {
        for (name, keys) in invocations(&read(client)) {
            let declared = schema
                .commands()
                .iter()
                .find(|c| c.name == name)
                .unwrap_or_else(|| panic!("{client}: {name:?} is not a command the schema has"));
            let expected: BTreeSet<&str> = declared.params.iter().map(|p| p.key.as_str()).collect();
            let sent: BTreeSet<&str> = keys.iter().map(String::as_str).collect();
            assert_eq!(
                sent, expected,
                "{client}: the arguments {name:?} is called with are not the ones \
                 the schema declares"
            );
            checked += 1;
        }
    }
    assert!(checked >= 8, "only {checked} invocations were checked");
    session.shutdown().expect("shutdown should succeed");
}

#[test]
fn the_schema_and_the_registry_declare_the_same_commands() {
    // Both directions. A command in the schema and not the registry is a typed
    // method that cannot be called; one in the registry and not the schema is
    // host surface no generated client can reach.
    let session = Session::open(FIXTURE).expect("the app fixture should open");
    let schema = duet_codegen::read_schema(session.fixture().text).expect("the fixture parses");
    let declared: BTreeSet<&str> = schema.commands().iter().map(|c| c.name.as_str()).collect();
    let registered: BTreeSet<&str> = session.commands().names().into_iter().collect();
    assert_eq!(
        declared, registered,
        "schema/app.json and duet_host_stdio::commands() disagree about which \
         commands exist"
    );
    assert!(!declared.is_empty(), "the fixture must declare commands");
    session.shutdown().expect("shutdown should succeed");
}

#[test]
fn the_scanner_reads_both_languages_and_finds_the_arguments_whole() {
    // Without this, a scanner that found nothing would make every check above
    // pass vacuously — and the emptiness assertions only catch the total case,
    // not a scanner that found the name and lost the keys.
    let dart = invocations(
        "        await client.invoke('bump', <String, DuetValue>{\n\
         \x20         'by': duetIntCodec.encode(by),\n\
         \x20         'path': duetStringCodec.encode(path),\n\
         \x20       }),\n",
    );
    assert_eq!(dart.len(), 1);
    assert_eq!(dart[0].0, "bump");
    assert_eq!(dart[0].1, ["by", "path"]);

    let ts = invocations(
        "      await this.client.invoke('bump', new Map<string, DuetValue>([\n        \
         ['by', duetIntCodec.encode(params.by)],\n        \
         ['path', duetStringCodec.encode(params.path)],\n      ])),\n",
    );
    assert_eq!(ts.len(), 1);
    assert_eq!(ts[0].0, "bump");
    assert_eq!(ts[0].1, ["by", "path"]);

    // A command with no arguments: the very next line is not an argument, so
    // the scan must stop immediately rather than swallowing the codecs below.
    let none =
        invocations("        await client.invoke('session.ping'),\n        duetDynamicCodec,\n");
    assert_eq!(none.len(), 1);
    assert_eq!(none[0].0, "session.ping");
    assert!(none[0].1.is_empty());
}
