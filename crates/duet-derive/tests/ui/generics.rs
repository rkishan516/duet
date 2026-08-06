// The schema names a struct once, and every instantiation would claim that one
// name.
use duet::SharedState;

#[derive(SharedState)]
struct Holder<T> {
    items: Vec<T>,
}

#[derive(SharedState)]
struct Borrowed<'a> {
    label: &'a str,
}

fn main() {}
