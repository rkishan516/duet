//! Parsing the shapes the Dart VM service actually announces.

use super::*;

#[test]
fn the_shape_the_engine_prints_with_auth_codes_on() {
    // Verbatim from a real run of this repository's own fixture.
    let url = VmServiceUrl::parse("http://127.0.0.1:56050/zuL-CgD5DQk=/")
        .expect("a real announcement should parse");
    assert_eq!(url.port(), 56050);
    assert_eq!(url.authority(), "127.0.0.1:56050");
    assert_eq!(url.websocket_path(), "/zuL-CgD5DQk=/ws");
    assert_eq!(url.websocket(), "ws://127.0.0.1:56050/zuL-CgD5DQk=/ws");
}

#[test]
fn the_shape_the_engine_prints_with_auth_codes_disabled() {
    // Also verbatim: `vm-service-port=45671,disable-service-auth-codes`
    // produced exactly this. Spike C's `replacen`-based conversion would have
    // produced `ws://127.0.0.1:45671/ws` here too — but only by accident, and
    // the case below is where it went wrong.
    let url = VmServiceUrl::parse("http://127.0.0.1:45671/").expect("should parse");
    assert_eq!(url.websocket(), "ws://127.0.0.1:45671/ws");
    assert_eq!(url.websocket_path(), "/ws");
}

#[test]
fn a_missing_trailing_slash_is_normalised_rather_than_producing_a_bad_path() {
    // The case string-splicing gets wrong: without normalisation this yields
    // `.../zuL-CgD5DQk=ws`, which the server answers with 404 and no
    // explanation of why.
    let url = VmServiceUrl::parse("http://127.0.0.1:56050/zuL-CgD5DQk=").expect("should parse");
    assert_eq!(url.websocket_path(), "/zuL-CgD5DQk=/ws");

    let bare = VmServiceUrl::parse("http://127.0.0.1:56050").expect("should parse");
    assert_eq!(bare.websocket_path(), "/ws");
}

#[test]
fn all_four_schemes_are_accepted_and_normalise_to_the_same_place() {
    // A developer pasting a `ws://` URI from an IDE, or an `https://` one from
    // a remote-device setup, should not have to convert it by hand.
    for uri in [
        "http://127.0.0.1:8181/abc/",
        "https://127.0.0.1:8181/abc/",
        "ws://127.0.0.1:8181/abc/",
        "wss://127.0.0.1:8181/abc/",
    ] {
        let url = VmServiceUrl::parse(uri).unwrap_or_else(|e| panic!("{uri} should parse: {e}"));
        assert_eq!(
            url.websocket(),
            "ws://127.0.0.1:8181/abc/ws",
            "{uri} should normalise"
        );
    }
}

#[test]
fn surrounding_whitespace_is_tolerated() {
    // The URL is sliced out of a log line, so a trailing `\r` from a pipe or a
    // stray space is entirely plausible.
    let url = VmServiceUrl::parse("  http://127.0.0.1:8181/abc/\r\n").expect("should parse");
    assert_eq!(url.port(), 8181);
}

#[test]
fn a_hostname_works_as_well_as_an_address() {
    // Not what the engine prints, but a developer forwarding a port would.
    let url = VmServiceUrl::parse("http://localhost:8181/x/").expect("should parse");
    assert_eq!(url.authority(), "localhost:8181");
}

#[test]
fn loopback_builds_the_shape_the_fixed_port_mode_produces() {
    // `engine_switches` disables auth codes, so this must match the
    // auth-code-free announcement exactly — the whole point of the fixed-port
    // route is that the URL is known without observing it.
    let built = VmServiceUrl::loopback(45671);
    let announced = VmServiceUrl::parse("http://127.0.0.1:45671/").expect("should parse");
    assert_eq!(
        built, announced,
        "the composed URL must equal the one the engine announces"
    );
    assert_eq!(built.to_string(), "http://127.0.0.1:45671/");
}

#[test]
fn a_uri_without_a_recognised_scheme_is_refused() {
    for bad in ["127.0.0.1:8181/", "ftp://127.0.0.1:8181/", "", "nonsense"] {
        assert!(
            matches!(VmServiceUrl::parse(bad), Err(UrlError::Scheme(_))),
            "{bad:?} has no usable scheme"
        );
    }
}

#[test]
fn a_uri_without_a_usable_port_is_refused() {
    // The VM service always announces a port. Defaulting to 80 would connect
    // somewhere unrelated and fail the handshake with a confusing message, so
    // this is refused up front.
    for bad in [
        "http://127.0.0.1/abc/",
        "http://127.0.0.1:/abc/",
        "http://127.0.0.1:notaport/",
        "http://127.0.0.1:99999/",
        "http://127.0.0.1:0/",
    ] {
        assert!(
            matches!(VmServiceUrl::parse(bad), Err(UrlError::Port(_))),
            "{bad:?} should be refused for its port"
        );
    }
}

#[test]
fn a_uri_with_no_host_is_refused() {
    assert_eq!(VmServiceUrl::parse("http://:8181/"), Err(UrlError::NoHost));
}

#[test]
fn every_rejection_explains_itself() {
    // These reach a developer through `DevError::VmService`.
    for bad in ["nonsense", "http://127.0.0.1/", "http://:8181/"] {
        let Err(e) = VmServiceUrl::parse(bad) else {
            panic!("{bad:?} should not parse");
        };
        assert!(
            e.to_string().len() > 20,
            "{bad:?} rejected with an unhelpful message: {e}"
        );
    }
}
