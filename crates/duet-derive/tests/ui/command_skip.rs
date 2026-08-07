// A command's arguments are the whole of its input, so there is nothing a
// skipped one could be filled from.
use duet::command;

#[command(skip)]
fn add(a: i64) -> i64 {
    a
}

fn main() {}
