// `u64` has no `SharedState` impl, so the derive's `<u64 as SharedState>` fails
// to resolve. The refusal is the missing impl, not a token the macro inspected.
use duet::SharedState;

#[derive(SharedState)]
struct App {
    counter: u64,
}

fn main() {}
