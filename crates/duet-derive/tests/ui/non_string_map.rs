// `Value::Map` is keyed by `String`; a map keyed by anything else would have to
// lower to a list of pairs, which destroys path addressing.
use std::collections::HashMap;

use duet::SharedState;

#[derive(SharedState)]
struct App {
    lookup: HashMap<i64, String>,
}

fn main() {}
