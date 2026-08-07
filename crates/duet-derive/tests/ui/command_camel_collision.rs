// Two distinct argument keys, one Dart and TypeScript parameter name.
use duet::command;

#[command]
fn resize(font_size: i64, #[duet(rename = "fontSize")] legacy: i64) -> i64 {
    font_size + legacy
}

fn main() {}
