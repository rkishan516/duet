// A typo that compiled would ship the field's Rust name as the wire key, with
// nothing to say so.
use duet::SharedState;

#[derive(SharedState)]
struct App {
    #[duet(renmae = "window_title")]
    title: String,
    #[duet(with = "my_codec")]
    counter: i64,
}

fn main() {}
