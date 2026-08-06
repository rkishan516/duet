// The store owns a `'static` tree, so a borrow cannot live in it. `&'static str`
// rather than `&'a str`, so this reaches the missing impl rather than the
// derive's own refusal of a generic type.
use duet::SharedState;

#[derive(SharedState)]
struct App {
    label: &'static str,
    values: &'static [i64],
}

fn main() {}
