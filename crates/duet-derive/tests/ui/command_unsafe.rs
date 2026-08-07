// The generated body calls the function, and a macro cannot assert a safety
// contract on a caller's behalf.
use duet::command;

#[command]
unsafe fn poke(a: i64) -> i64 {
    a
}

fn main() {}
