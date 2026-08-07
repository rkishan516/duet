// A command is reached by name from a guest, with no receiver for it to be
// called on and nothing for the host to resolve one from.
use duet::command;

struct Editor;

impl Editor {
    #[command]
    fn zoom(&self, by: i64) -> i64 {
        by
    }
}

fn main() {}
