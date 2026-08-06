// Two distinct wire keys, one Dart and TypeScript accessor.
use duet::SharedState;

#[derive(SharedState)]
struct Editor {
    font_size: i64,
    #[duet(rename = "fontSize")]
    legacy: i64,
}

fn main() {}
