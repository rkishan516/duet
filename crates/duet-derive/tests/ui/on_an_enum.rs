// The schema's type language has no arm for a choice between shapes. A domain
// enum is the documented case for a hand-written impl.
use duet::SharedState;

#[derive(SharedState)]
enum Theme {
    Light,
    Dark,
}

fn main() {}
