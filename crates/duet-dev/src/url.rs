//! The Dart VM service's address, parsed rather than string-spliced.
//!
//! The engine announces something like
//! `http://127.0.0.1:56050/zuL-CgD5DQk=/` and the WebSocket endpoint is that
//! same base with `ws` appended and the scheme swapped. Spike C did this with
//! two `replacen`/`format!` calls, which works right up to the first URI that
//! is shaped slightly differently — no trailing slash, or auth codes disabled
//! (`http://127.0.0.1:45671/`), which is exactly the shape this crate's own
//! fixed-port mode produces.
//!
//! So it is parsed. This is a deliberately *narrow* parser: it accepts the
//! `http`/`https`/`ws`/`wss` origin-plus-path shape the VM service actually
//! announces and refuses everything else, rather than being a general URL
//! implementation. A dev tool connecting to a loopback socket does not need
//! userinfo, query strings, fragments, IPv6 zone identifiers or percent
//! decoding, and a parser that pretended to handle them would be claiming
//! correctness it has no tests for.

use std::fmt;

/// A parsed Dart VM service address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmServiceUrl {
    /// Host, as written — `127.0.0.1` in every case this tool produces.
    host: String,
    /// TCP port.
    port: u16,
    /// The path, always starting with `/` and always ending with `/`.
    ///
    /// `/` when auth codes are disabled, `/<authcode>/` otherwise. Normalising
    /// the trailing slash here is what lets [`VmServiceUrl::websocket`] be a
    /// plain concatenation.
    path: String,
}

/// Why a URI could not be understood.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum UrlError {
    /// The scheme was missing or not one of `http`, `https`, `ws`, `wss`.
    Scheme(String),
    /// There was no host between the scheme and the path.
    NoHost,
    /// The port was absent, non-numeric, or out of range.
    Port(String),
}

impl fmt::Display for UrlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UrlError::Scheme(found) => write!(
                f,
                "expected an http://, https://, ws:// or wss:// URI, found {found:?}"
            ),
            UrlError::NoHost => f.write_str("the URI has no host between the scheme and the port"),
            UrlError::Port(found) => write!(
                f,
                "the URI needs an explicit port and {found:?} is not one \
                 (the Dart VM service always announces one)"
            ),
        }
    }
}

impl std::error::Error for UrlError {}

impl VmServiceUrl {
    /// Parses the URI the engine announces.
    ///
    /// Accepts it with or without a trailing slash, with or without an auth
    /// code, and under any of the four schemes — the VM service prints
    /// `http://`, this crate's own fixed-port mode composes the same shape,
    /// and a developer pasting a `ws://` URI from an IDE should not have to
    /// care.
    ///
    /// # Errors
    ///
    /// [`UrlError`] if the scheme, host or port is missing or malformed.
    pub fn parse(uri: &str) -> Result<Self, UrlError> {
        let uri = uri.trim();
        let rest = ["http://", "https://", "ws://", "wss://"]
            .iter()
            .find_map(|scheme| uri.strip_prefix(scheme))
            .ok_or_else(|| UrlError::Scheme(uri.chars().take(16).collect()))?;

        // Split authority from path at the first `/`. No `?`/`#` handling: the
        // VM service never announces either, and silently discarding one would
        // be worse than never seeing it.
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };

        let (host, port) = authority
            .rsplit_once(':')
            .ok_or_else(|| UrlError::Port(authority.to_string()))?;
        if host.is_empty() {
            return Err(UrlError::NoHost);
        }
        let port: u16 = port.parse().map_err(|_| UrlError::Port(port.to_string()))?;
        if port == 0 {
            return Err(UrlError::Port(port.to_string()));
        }

        let mut path = path.to_string();
        if !path.ends_with('/') {
            path.push('/');
        }

        Ok(VmServiceUrl {
            host: host.to_string(),
            port,
            path,
        })
    }

    /// Builds the URL for a loopback VM service on `port` with auth codes
    /// disabled — the shape [`crate::engine_switches`] produces.
    pub fn loopback(port: u16) -> Self {
        VmServiceUrl {
            host: "127.0.0.1".to_string(),
            port,
            path: "/".to_string(),
        }
    }

    /// `host:port`, for [`std::net::TcpStream::connect`].
    pub(crate) fn authority(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    /// The path to `GET` in the WebSocket handshake — the base path plus `ws`.
    pub(crate) fn websocket_path(&self) -> String {
        format!("{}ws", self.path)
    }

    /// The full `ws://` URL, for logs and for a developer to paste into an IDE.
    pub fn websocket(&self) -> String {
        format!("ws://{}:{}{}ws", self.host, self.port, self.path)
    }

    /// The port this address points at.
    pub fn port(&self) -> u16 {
        self.port
    }
}

impl fmt::Display for VmServiceUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "http://{}:{}{}", self.host, self.port, self.path)
    }
}

#[cfg(test)]
#[path = "url_tests.rs"]
mod tests;
