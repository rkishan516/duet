// A typo in `#[command(renmae = "...")]` that compiled would ship the wrong
// command name.
use duet::command;

#[command(renmae = "add")]
fn add(a: i64) -> i64 {
    a
}

fn main() {}
