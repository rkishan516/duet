// A skipped field is not on the wire, so decoding the struct back out of the
// store has nothing to read it from and uses `Default`.
use duet::SharedState;

struct Cache(i64);

#[derive(SharedState)]
struct App {
    counter: i64,
    #[duet(skip)]
    cache: Cache,
}

fn main() {}
