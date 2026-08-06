// A wire key is one path segment. `editor.zoom` would be read back as two.
use duet::SharedState;

#[derive(SharedState)]
struct App {
    #[duet(rename = "editor.zoom")]
    zoom: f64,
    #[duet(rename = "")]
    nothing: i64,
}

fn main() {}
