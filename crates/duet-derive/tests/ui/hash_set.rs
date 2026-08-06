// A set's iteration order is not a function of its contents, so `to_value`
// would not be a function of the value either.
use std::collections::HashSet;

use duet::SharedState;

#[derive(SharedState)]
struct App {
    tags: HashSet<String>,
}

fn main() {}
