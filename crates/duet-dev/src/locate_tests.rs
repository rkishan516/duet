//! VM service discovery — the fd-1 replacement.

use super::*;

#[test]
fn the_engine_switch_variables_are_the_shape_the_embedder_reads() {
    // The embedder reads a count from `FLUTTER_ENGINE_SWITCHES` then
    // `FLUTTER_ENGINE_SWITCH_1..N`, and prepends `--` itself. A leading `--`
    // here would produce `----vm-service-port` and be silently ignored — the
    // engine would pick a random port and the driver would connect nowhere.
    let switches = engine_switches(45671);
    assert_eq!(switches.len(), 3, "a count plus two switches");
    assert_eq!(
        switches[0],
        ("FLUTTER_ENGINE_SWITCHES".to_string(), "2".to_string()),
        "the count must match the number of switches that follow"
    );
    assert_eq!(switches[1].0, "FLUTTER_ENGINE_SWITCH_1");
    assert_eq!(switches[1].1, "vm-service-port=45671");
    assert_eq!(switches[2].0, "FLUTTER_ENGINE_SWITCH_2");
    assert_eq!(switches[2].1, "disable-service-auth-codes");

    for (name, value) in &switches[1..] {
        assert!(
            !value.starts_with('-'),
            "{name}={value} must not carry its own dashes"
        );
    }
}

#[test]
fn the_switches_and_the_url_builder_agree_on_the_port() {
    // These two are used together and nothing else checks they match: the
    // switches tell the engine where to listen, `loopback` says where to
    // connect. A mismatch is a connect timeout with no explanation.
    let port = 51234;
    let switches = engine_switches(port);
    assert!(
        switches
            .iter()
            .any(|(_, v)| v == &format!("vm-service-port={port}"))
    );
    assert_eq!(crate::VmServiceUrl::loopback(port).port(), port);
}

#[test]
fn a_free_port_is_actually_bindable_afterwards() {
    // The point of the helper: the port it returns must be usable by the
    // engine a moment later.
    let port = free_port().expect("a free port should be available");
    assert_ne!(port, 0, "port 0 means the OS assigned nothing");
    let listener = std::net::TcpListener::bind(("127.0.0.1", port))
        .expect("the port should still be bindable right after being released");
    drop(listener);
}

#[test]
fn successive_free_ports_differ() {
    // Not guaranteed by the OS, but a helper that returned the same port twice
    // in a row would mean two engines fighting over it.
    let a = free_port().expect("first");
    let b = free_port().expect("second");
    assert_ne!(
        a, b,
        "the OS should not hand out the same ephemeral port twice"
    );
}

#[test]
fn the_announcement_this_engine_actually_prints_is_recognised() {
    // Captured verbatim from a run of this repository's fixture, in both the
    // auth-code and no-auth-code shapes.
    let scanner = Announcement;
    let cases = [
        (
            "flutter: The Dart VM service is listening on http://127.0.0.1:56050/zuL-CgD5DQk=/",
            "ws://127.0.0.1:56050/zuL-CgD5DQk=/ws",
        ),
        (
            "flutter: The Dart VM service is listening on http://127.0.0.1:45671/",
            "ws://127.0.0.1:45671/ws",
        ),
        // Without the `flutter: ` prefix, which is how it arrives when the
        // guest's output is not routed through Flutter's own logger.
        (
            "The Dart VM service is listening on http://127.0.0.1:8181/abc/",
            "ws://127.0.0.1:8181/abc/ws",
        ),
        // The older wording, in case an SDK still uses it.
        (
            "Observatory listening on http://127.0.0.1:8181/xyz/",
            "ws://127.0.0.1:8181/xyz/ws",
        ),
    ];
    for (line, want) in cases {
        let found = scanner
            .read(line)
            .unwrap_or_else(|| panic!("{line:?} should be recognised"));
        assert_eq!(found.websocket(), want);
    }
}

#[test]
fn a_trailing_carriage_return_or_extra_text_does_not_break_it() {
    // Lines come off a pipe, so `\r\n` is entirely possible, and the engine
    // has appended text after the URL before.
    let scanner = Announcement;
    for line in [
        "The Dart VM service is listening on http://127.0.0.1:8181/k/\r",
        "The Dart VM service is listening on http://127.0.0.1:8181/k/ (press h for help)",
    ] {
        let found = scanner.read(line).unwrap_or_else(|| panic!("{line:?}"));
        assert_eq!(found.websocket(), "ws://127.0.0.1:8181/k/ws");
    }
}

#[test]
fn an_unrelated_url_in_the_guests_own_output_is_not_mistaken_for_the_service() {
    // A guest printing a URL is completely ordinary, and connecting to it
    // instead of the VM service would fail the handshake with a confusing
    // message — or worse, succeed against something unrelated.
    let scanner = Announcement;
    for line in [
        "flutter: fetching http://127.0.0.1:3000/api/users",
        "I/flutter: my server is at http://127.0.0.1:8080/",
        "The Dart VM service is listening on <nothing here>",
        "listening on port 8080",
        "",
    ] {
        assert!(
            scanner.read(line).is_none(),
            "{line:?} must not be read as an announcement"
        );
    }
}

#[test]
fn a_line_that_mentions_listening_but_has_an_unparseable_url_is_rejected() {
    // Better to keep waiting for a real announcement than to hand a malformed
    // URL to the connector.
    let scanner = Announcement;
    assert!(
        scanner
            .read("The Dart VM service is listening on http://127.0.0.1/")
            .is_none(),
        "a URL with no port is not usable"
    );
}
