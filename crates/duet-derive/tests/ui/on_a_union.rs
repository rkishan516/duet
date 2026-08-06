use duet::SharedState;

#[derive(SharedState)]
union Raw {
    signed: i64,
    floating: f64,
}

fn main() {}
