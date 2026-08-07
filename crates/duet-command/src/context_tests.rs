//! Tests for [`CommandContext`](super::CommandContext) and
//! [`FromContext`](super::FromContext).

use super::*;
use duet_core::{Path, Value};
use duet_runtime::{NullSink, Runtime};

#[test]
fn the_context_hands_back_a_working_store() {
    let runtime = Runtime::spawn(Value::map([("count", Value::Int(1))]), NullSink);
    let context = CommandContext::new(runtime.handle());
    let path = Path::parse("count").expect("test path should parse");

    assert_eq!(
        context.store().get(&path).expect("read should succeed"),
        Some(Value::Int(1))
    );
    context
        .store()
        .set(&path, Value::Int(2))
        .expect("write should succeed");
    assert_eq!(
        runtime.handle().get(&path).expect("read should succeed"),
        Some(Value::Int(2)),
        "the context's handle must address the same store every other handle does"
    );
    runtime.shutdown().expect("shutdown should succeed");
}

#[test]
fn from_context_hands_back_the_very_context_it_was_given() {
    // Not a copy: a command body holding `&CommandContext` must be looking at
    // the invocation's own context, or a future capability hung on it would be
    // read from the wrong place.
    let runtime = Runtime::spawn(Value::Null, NullSink);
    let context = CommandContext::new(runtime.handle());
    let borrowed = <&CommandContext as FromContext>::from_context(&context);
    assert!(std::ptr::eq(borrowed, &context));
    runtime.shutdown().expect("shutdown should succeed");
}

#[test]
fn a_context_can_cross_a_thread() {
    // `CommandEntry` is `Copy` and a registry built from one is shared between
    // surfaces, so the context a body receives must not be the thing that pins
    // a command to one thread.
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<CommandContext>();
}
