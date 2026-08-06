// Two handles to one node become two independent copies once they are values in
// the tree, so shared-mutability wrappers are refused rather than silently
// duplicated.
use std::cell::RefCell;
use std::rc::Rc;

use duet::SharedState;

#[derive(SharedState)]
struct App {
    shared: Rc<i64>,
    mutable: RefCell<i64>,
}

fn main() {}
