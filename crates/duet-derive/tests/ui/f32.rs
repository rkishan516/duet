// `f32` is lossless out and lossy in: Dart and TypeScript have no 32-bit float.
use duet::SharedState;

#[derive(SharedState)]
struct App {
    ratio: f32,
}

fn main() {}
