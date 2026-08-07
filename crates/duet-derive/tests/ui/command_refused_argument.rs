// `u64` has no `SharedState` impl, so `<u64 as CommandParam>` fails to resolve.
// The refusal is the missing impl, not a token the macro inspected.
use duet::command;

#[command]
fn add(a: u64) -> i64 {
    a as i64
}

fn main() {}
