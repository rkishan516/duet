// The case that would defeat any token inspection: the macro sees the alias,
// never the type it resolves to. Trait resolution runs after type resolution,
// so the alias is transparent and the rejection still stands.
use duet::command;

type Sneaky = u64;

#[command]
fn add(a: Sneaky) -> i64 {
    a as i64
}

fn main() {}
