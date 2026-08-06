// Two fields on one key occupy one map entry: one of them is unreachable.
use duet::SharedState;

#[derive(SharedState)]
struct App {
    title: String,
    #[duet(rename = "title")]
    heading: String,
}

fn main() {}
