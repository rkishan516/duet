// There is no async runtime anywhere in Duet, deliberately: a command body runs
// on the thread that called `dispatch_with`, which on macOS also drives the UI.
use duet::command;

#[command]
async fn slow(a: i64) -> i64 {
    a
}

fn main() {}
