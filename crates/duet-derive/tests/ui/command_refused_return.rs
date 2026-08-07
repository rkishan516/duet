// The same rejection in return position: `CommandReturn`'s four impls all
// require `SharedState`, and `u64` has no impl.
use duet::command;

#[command]
fn count() -> u64 {
    0
}

fn main() {}
