// `OsString` is WTF-8 on Windows and `Value::Str` is UTF-8 only, so a path has
// no lossless spelling on the wire.
use std::path::PathBuf;

use duet::SharedState;

#[derive(SharedState)]
struct App {
    location: PathBuf,
}

fn main() {}
