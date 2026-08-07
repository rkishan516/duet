// An argument key is a parameter *name*, so a pattern has none to use.
use duet::command;

#[command]
fn span((from, to): (i64, i64)) -> i64 {
    to - from
}

fn main() {}
