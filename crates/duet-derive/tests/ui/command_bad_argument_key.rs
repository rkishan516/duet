// An argument key is one segment of a `Value::Map`, and a generated client
// names a parameter after it.
use duet::command;

#[command]
fn bump(#[duet(rename = "a.b")] by: i64) -> i64 {
    by
}

fn main() {}
