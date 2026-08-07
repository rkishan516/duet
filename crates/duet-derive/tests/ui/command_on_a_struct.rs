// `#[command]` describes a function, and this is not one.
use duet::command;

#[command]
struct Add {
    a: i64,
}

fn main() {}
