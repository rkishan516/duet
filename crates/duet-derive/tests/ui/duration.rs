// There is no canonical wire spelling for a duration, and choosing one silently
// is worse than making the developer choose.
use std::time::Duration;

use duet::SharedState;

#[derive(SharedState)]
struct App {
    elapsed: Duration,
}

fn main() {}
