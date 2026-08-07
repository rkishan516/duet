// Two arguments on one key occupy one entry of the `args` map: one of the two
// can never be supplied.
use duet::command;

#[command]
fn bump(by: i64, #[duet(rename = "by")] amount: i64) -> i64 {
    by + amount
}

fn main() {}
