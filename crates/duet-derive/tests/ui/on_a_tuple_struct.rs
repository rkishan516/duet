// A wire key is a field name, and a tuple struct has none.
use duet::SharedState;

#[derive(SharedState)]
struct Millis(i64);

fn main() {}
