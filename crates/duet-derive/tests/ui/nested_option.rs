// `Some(None)` and `None` both lower to `Value::Null`, so the two are
// indistinguishable and `Option<T>` requires `T: NotNullable`.
use duet::SharedState;

#[derive(SharedState)]
struct App {
    maybe: Option<Option<i64>>,
}

fn main() {}
