// A command name has to be a legal identifier in Dart and TypeScript too,
// because a generated client turns it into a method.
use duet::command;

#[command(rename = "2fast")]
fn go() {}

fn main() {}
