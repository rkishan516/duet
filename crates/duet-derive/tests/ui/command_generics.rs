// The schema names a command once, and every instantiation of a generic would
// claim that one name with a different argument shape.
use duet::command;

#[command]
fn identity<T>(a: T) -> T {
    a
}

fn main() {}
