// A command takes its arguments by value and its context by reference. `&str`
// is neither: the store owns a `'static` tree, so nothing can be borrowed from
// it, and `&str` is not `&CommandContext`.
use duet::command;

#[command]
fn label(title: &str) -> String {
    title.to_string()
}

fn main() {}
