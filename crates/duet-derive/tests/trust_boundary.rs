//! Codegen must not reopen the guest trust boundary.
//!
//! # What this file is guarding
//!
//! Authorization in Duet is **by construction**. An `invoke` carries an id, a
//! command name and arguments — and no caller identity of any kind. A surface
//! reaches exactly the commands the embedder put in the `CommandHost` it was
//! built with, so a webview running untrusted content simply has no name for a
//! command its own host does not hold. There is nothing for a guest to spoof,
//! because there is nothing it is asked to assert.
//!
//! Making commands **declarative** is exactly the change that could quietly
//! undo that. A `#[command(role = "admin")]` or a `caller: &Principal`
//! parameter would look like an ergonomic addition and would in fact move the
//! decision from the embedder's registry to a field the guest fills in. The
//! moment `Request::Invoke` carries a fourth field, that becomes possible; while
//! it carries three, it does not.
//!
//! # Why this is a compile-time check and not an assertion
//!
//! A runtime test would have to know what a caller identity looks like in order
//! to assert its absence, and the whole point is that nobody has designed one
//! yet. An exhaustive destructuring needs no such knowledge: adding **any**
//! field to `Request::Invoke` — whatever it is called, whatever it holds —
//! stops this file compiling, and a test that does not compile is a failure and
//! not a skip.

use duet_core::Value;
use duet_protocol::{Args, Request, RequestId};

#[test]
fn invoke_carries_an_id_a_name_and_arguments_and_nothing_else() {
    let request = Request::Invoke {
        id: RequestId(1),
        command: "subtract".to_string(),
        args: Args::from([("a".to_string(), Value::Int(10))]),
    };

    // No `..`, deliberately. This is the assertion: a fourth field on `Invoke`
    // fails to compile here, and whoever added it has to come and read the
    // paragraph above before they can make it pass.
    let Request::Invoke { id, command, args } = request else {
        panic!("the request just built is an Invoke");
    };

    assert_eq!(id, RequestId(1));
    assert_eq!(command, "subtract");
    assert_eq!(args.len(), 1);
}

#[test]
fn two_registries_over_one_store_still_decide_what_a_surface_may_reach() {
    // The property the paragraph above describes, exercised through the
    // declarative path: nothing about the two requests differs — same command
    // name, same arguments, same store — so the only thing deciding the outcome
    // is which registry the surface was built with.
    use duet::{CommandContext, CommandEntry, Commands, command, commands};
    use duet_core::SubscriberId;
    use duet_protocol::{Response, dispatch_with};
    use duet_runtime::{NullSink, Runtime};

    #[command(rename = "secret.rotate_keys")]
    fn rotate(_ctx: &CommandContext) -> bool {
        true
    }

    #[command(rename = "documents.rename")]
    fn rename_document() {}

    static PRIVILEGED: [CommandEntry; 1] = commands![rotate];
    static SANDBOXED: [CommandEntry; 1] = commands![rename_document];

    let runtime = Runtime::spawn(Value::Null, NullSink);
    let ask = |commands: &Commands, id: u64| {
        dispatch_with(
            &runtime.handle(),
            SubscriberId(1),
            commands,
            Request::Invoke {
                id: RequestId(id),
                command: "secret.rotate_keys".to_string(),
                args: Args::new(),
            },
        )
    };

    assert!(matches!(
        ask(&Commands::from_entries(&PRIVILEGED), 1),
        Response::Returned { .. }
    ));
    assert!(
        matches!(
            ask(&Commands::from_entries(&SANDBOXED), 2),
            Response::Failed { .. }
        ),
        "the sandboxed surface must not reach the privileged surface's command"
    );
    runtime.shutdown().expect("shutdown should succeed");
}
