//! The typed layer against a real running store.
//!
//! Everything here goes through [`Runtime`], not a stub: the behaviours worth
//! pinning — a write refused because its parent is `Null`, a subscription that
//! succeeds on a path a read says is absent — are behaviours of the actual
//! store, and a fake would only reproduce whatever this crate already assumed.

use duet_core::{MAX_VALUE_DEPTH, Path, SubscriberId, Value};
use duet_runtime::{RecordingSink, Runtime, RuntimeError, StoreHandle};
use duet_schema::{
    Bytes, DecodeError, FieldDef, FieldError, InstallError, NotNullable, Reading, Registry, Schema,
    SharedState, Ty, install,
};

mod fixture;
use fixture::{AppState, Editor};

/// Spawns a runtime seeded with `state` and returns it with a typed view.
fn spawn(state: AppState) -> (Runtime, duet_schema::TypedStore) {
    let runtime = Runtime::spawn(Value::Null, RecordingSink::new());
    let store = install(runtime.handle(), &state).expect("AppState installs");
    (runtime, store)
}

#[test]
fn install_seeds_every_schema_field_so_each_one_is_writable() {
    // `Store::set` never creates intermediate nodes, so a field is writable
    // only if its ancestors already exist. This is what makes "key absent" a
    // schema violation rather than an ordinary state.
    let (runtime, _store) = spawn(AppState::sample());
    let handle = runtime.handle();

    for literal in ["counter", "title", "editor", "tags", "thumbnail"] {
        let path = Path::parse(literal).expect("literal parses");
        assert!(
            handle.get(&path).expect("store is up").is_some(),
            "{literal} must be materialized by install"
        );
    }
    runtime.shutdown().expect("clean shutdown");
}

#[test]
fn a_required_field_round_trips_through_the_store() {
    let (runtime, store) = spawn(AppState::sample());
    let counter = store.field::<i64>("counter").expect("legal literal");

    assert_eq!(counter.get().expect("store is up"), Reading::Present(0));
    counter.set(&41).expect("a plain write");
    assert_eq!(counter.get().expect("store is up"), Reading::Present(41));
    runtime.shutdown().expect("clean shutdown");
}

#[test]
fn a_field_keeps_its_literal_and_its_parsed_path_in_agreement() {
    let (runtime, store) = spawn(AppState::sample());
    let zoom = store.field::<f64>("editor.zoom").expect("legal literal");
    assert_eq!(zoom.literal(), "editor.zoom");
    assert_eq!(zoom.path().to_string(), "editor.zoom");
    assert_eq!(zoom.path().segments().len(), 2);
    runtime.shutdown().expect("clean shutdown");
}

#[test]
fn a_malformed_literal_fails_at_wiring_time_not_on_the_first_read() {
    let (runtime, store) = spawn(AppState::sample());
    assert!(store.field::<i64>("counter.").is_err());
    assert!(store.optional_field::<String>("a..b").is_err());
    runtime.shutdown().expect("clean shutdown");
}

#[test]
fn an_optional_field_keeps_none_and_absent_apart() {
    let (runtime, store) = spawn(AppState::sample());
    let title = store.optional_field::<String>("title").expect("legal");

    // Seeded as `None`, which is `Value::Null` at an existing key.
    assert_eq!(title.get().expect("store is up"), Reading::None);

    title
        .set(Some(&"draft".to_string()))
        .expect("writing over a null");
    assert_eq!(
        title.get().expect("store is up"),
        Reading::Present("draft".to_string())
    );

    title.set(None).expect("writing a null back");
    assert_eq!(title.get().expect("store is up"), Reading::None);

    // Nothing was ever written here, so it is absent rather than null.
    let ghost = store.optional_field::<String>("nowhere").expect("legal");
    assert_eq!(ghost.get().expect("store is up"), Reading::Absent);
    runtime.shutdown().expect("clean shutdown");
}

#[test]
fn a_null_struct_makes_its_children_behave_three_different_ways_at_once() {
    // Measured against the real host, and the reason `OptionalField` does not
    // paper over any of the three: with `editor` set to `None`,
    //   get       -> nothing (the typed layer reports Absent)
    //   subscribe -> succeeds
    //   set       -> fails, "addresses the wrong kind of node"
    let (runtime, store) = spawn(AppState {
        editor: None,
        ..AppState::sample()
    });
    let handle = runtime.handle();
    let zoom = store.field::<f64>("editor.zoom").expect("legal");

    assert_eq!(zoom.get().expect("store is up"), Reading::Absent);

    let subscriber = handle.next_subscriber_id();
    let (_, snapshot) = zoom.subscribe(subscriber).expect("subscribe succeeds");
    assert_eq!(snapshot, Reading::Absent);

    let refused = zoom
        .set(&2.0)
        .expect_err("a write through a null must fail");
    assert!(
        refused.to_string().contains("wrong kind of node"),
        "got {refused}"
    );
    runtime.shutdown().expect("clean shutdown");
}

#[test]
fn a_value_another_guest_wrote_is_a_mismatch_not_an_error() {
    // The two-guest reality: a webview and a Flutter engine write one store.
    // A typed reader meeting the wrong type reports it as data.
    let (runtime, store) = spawn(AppState::sample());
    let handle = runtime.handle();
    let counter = store.field::<i64>("counter").expect("legal");

    write_raw(&handle, "counter", Value::Str("not a number".into()));

    match counter.get().expect("store is up") {
        Reading::Mismatch { found, error } => {
            assert_eq!(found, Value::Str("not a number".into()));
            assert_eq!(error.to_string(), "expected i64 at <root>, found string");
        }
        other => panic!("expected a mismatch, got {other:?}"),
    }
    runtime.shutdown().expect("clean shutdown");
}

#[test]
fn a_null_at_a_required_path_is_a_mismatch_and_at_an_optional_one_is_none() {
    let (runtime, store) = spawn(AppState::sample());
    let handle = runtime.handle();
    write_raw(&handle, "counter", Value::Null);

    let required = store.field::<i64>("counter").expect("legal");
    assert!(required.get().expect("store is up").is_mismatch());

    let optional = store.optional_field::<i64>("counter").expect("legal");
    assert_eq!(optional.get().expect("store is up"), Reading::None);
    runtime.shutdown().expect("clean shutdown");
}

#[test]
fn a_subscription_snapshot_is_a_reading_like_any_other() {
    let (runtime, store) = spawn(AppState::sample());
    let handle = runtime.handle();
    write_raw(&handle, "counter", Value::Bool(true));

    let counter = store.field::<i64>("counter").expect("legal");
    let (subscription, snapshot) = counter
        .subscribe(handle.next_subscriber_id())
        .expect("subscribe succeeds");
    assert!(
        snapshot.is_mismatch(),
        "a subscription that starts on the wrong type says so immediately"
    );

    let optional = store.optional_field::<i64>("counter").expect("legal");
    let (_, optional_snapshot) = optional
        .subscribe(handle.next_subscriber_id())
        .expect("subscribe succeeds");
    assert!(optional_snapshot.is_mismatch());

    assert_ne!(subscription, duet_core::SubscriptionId(u64::MAX));
    runtime.shutdown().expect("clean shutdown");
}

#[test]
fn a_write_past_the_stores_depth_limit_is_named_rather_than_surprising() {
    // Typed as `Value` — the `dynamic` arm — so the test can express a value 61
    // containers deep without writing 61 nested `Vec`s as a Rust type. It is
    // still `Field::set`'s own guard that runs.
    let (runtime, store) = spawn(AppState::sample());
    let tags = store.field::<Value>("tags").expect("legal literal");

    // `tags` is one segment down, so a value of depth `MAX_VALUE_DEPTH` there
    // would leave the store at `MAX_VALUE_DEPTH + 1`.
    assert!(
        tags.set(&nested_lists(MAX_VALUE_DEPTH - 1)).is_ok(),
        "exactly at the limit must be accepted"
    );

    match tags.set(&nested_lists(MAX_VALUE_DEPTH)) {
        Err(FieldError::TooDeep { path, depth, max }) => {
            assert_eq!(path.to_string(), "tags");
            assert_eq!(depth, MAX_VALUE_DEPTH + 1);
            assert_eq!(max, MAX_VALUE_DEPTH);
        }
        other => panic!("expected a named TooDeep, got {other:?}"),
    }

    // The refusal is the typed layer's, before any round trip, and it agrees
    // with what the store itself would have said.
    let store_side = runtime
        .handle()
        .set(&Path::parse("tags").unwrap(), nested_lists(MAX_VALUE_DEPTH));
    assert!(store_side.is_err(), "the store refuses the same write");

    // An optional field guards its writes identically.
    let optional = store.optional_field::<Vec<String>>("tags").expect("legal");
    let deep: Vec<String> = Vec::new();
    assert!(optional.set(Some(&deep)).is_ok());
    runtime.shutdown().expect("clean shutdown");
}

#[test]
fn a_typed_call_after_shutdown_reports_the_store_is_gone() {
    let (runtime, store) = spawn(AppState::sample());
    let counter = store.field::<i64>("counter").expect("legal");
    runtime.shutdown().expect("clean shutdown");

    assert_eq!(counter.get(), Err(RuntimeError::CoreThreadGone));
    assert_eq!(
        counter.set(&1),
        Err(FieldError::Store(RuntimeError::CoreThreadGone))
    );
    assert_eq!(
        store
            .optional_field::<String>("title")
            .expect("legal")
            .set(None),
        Err(FieldError::Store(RuntimeError::CoreThreadGone))
    );
    let subscriber = SubscriberId(0);
    assert_eq!(
        counter.subscribe(subscriber).map(|(id, _)| id),
        Err(RuntimeError::CoreThreadGone)
    );
    assert_eq!(
        store
            .optional_field::<String>("title")
            .expect("legal")
            .subscribe(subscriber)
            .map(|(id, _)| id),
        Err(RuntimeError::CoreThreadGone)
    );
    assert_eq!(
        store
            .optional_field::<String>("title")
            .expect("legal")
            .get(),
        Err(RuntimeError::CoreThreadGone)
    );
}

#[test]
fn install_refuses_a_root_whose_schema_is_invalid() {
    // A recursive type has no finite set of paths, so no client could be
    // generated for it. Finding that at startup is the point.
    struct Node;

    impl SharedState for Node {
        fn to_value(&self) -> Value {
            Value::map([])
        }

        fn from_value(_: &Value) -> Result<Self, DecodeError> {
            Ok(Node)
        }

        fn schema(registry: &mut Registry) -> Ty {
            registry.define::<Self>("Node", |r| {
                vec![FieldDef::new("next", Node::schema(r).optional())]
            })
        }
    }

    impl NotNullable for Node {}

    let runtime = Runtime::spawn(Value::Null, RecordingSink::new());
    match install(runtime.handle(), &Node) {
        Err(InstallError::Schema(errors)) => {
            assert_eq!(errors.to_string(), "recursive type: Node -> Node");
        }
        other => panic!("expected a schema refusal, got {other:?}"),
    }
    // Nothing was written: the schema is checked before the seed.
    assert_eq!(
        runtime.handle().get(&Path::root()).expect("store is up"),
        Some(Value::Null)
    );
    runtime.shutdown().expect("clean shutdown");
}

#[test]
fn install_reports_a_store_that_is_already_gone() {
    let runtime = Runtime::spawn(Value::Null, RecordingSink::new());
    let handle = runtime.handle();
    runtime.shutdown().expect("clean shutdown");
    match install(handle, &AppState::sample()) {
        Err(InstallError::Store(RuntimeError::CoreThreadGone)) => {}
        other => panic!("expected a store refusal, got {other:?}"),
    }
}

#[test]
fn the_typed_store_exposes_the_raw_handle_for_what_it_does_not_wrap() {
    let (runtime, store) = spawn(AppState::sample());
    let subscriber = store.handle().next_subscriber_id();
    let counter = store.field::<i64>("counter").expect("legal");
    let (subscription, _) = counter.subscribe(subscriber).expect("subscribe");
    assert!(
        store
            .handle()
            .unsubscribe(subscriber, subscription)
            .expect("store is up")
    );
    assert_eq!(
        counter.handle().get(&Path::parse("counter").unwrap()),
        Ok(Some(Value::Int(0)))
    );
    runtime.shutdown().expect("clean shutdown");
}

#[test]
fn the_whole_root_decodes_back_to_the_value_that_was_installed() {
    // The end-to-end property: `install` writes what `AppState::from_value`
    // reads, through a real store, with no intermediate representation.
    let original = AppState::sample();
    let (runtime, _) = spawn(original.clone());
    let root = runtime
        .handle()
        .get(&Path::root())
        .expect("store is up")
        .expect("the root exists");
    assert_eq!(AppState::from_value(&root), Ok(original));
    runtime.shutdown().expect("clean shutdown");
}

#[test]
fn the_schema_of_the_fixture_is_what_the_emitters_will_read() {
    let schema = Schema::of::<AppState>().expect("AppState is a valid schema");
    assert_eq!(
        schema
            .types()
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>(),
        ["AppState", "Editor"]
    );
    assert_eq!(schema.depth(), 2);
    assert!(schema.render().contains("\"key\": \"thumbnail\""));
}

// --- Helpers ---

/// Writes `value` at `literal` through the untyped handle, standing in for
/// another guest.
fn write_raw(handle: &StoreHandle, literal: &str, value: Value) {
    let path = Path::parse(literal).expect("literal parses");
    handle.set(&path, value).expect("the raw write succeeds");
}

/// `depth` nested lists around a single string.
fn nested_lists(depth: usize) -> Value {
    let mut value = Value::Str("leaf".to_string());
    for _ in 0..depth {
        value = Value::List(vec![value]);
    }
    value
}

/// Round-trips `Bytes` through the typed layer, which nothing else here covers.
#[test]
fn a_bytes_field_round_trips() {
    let (runtime, store) = spawn(AppState::sample());
    let thumbnail = store.field::<Bytes>("thumbnail").expect("legal");
    assert_eq!(
        thumbnail.get().expect("store is up"),
        Reading::Present(Bytes(vec![0xDE, 0xAD]))
    );
    thumbnail.set(&Bytes(vec![1, 2, 3])).expect("writing bytes");
    assert_eq!(
        thumbnail.get().expect("store is up"),
        Reading::Present(Bytes(vec![1, 2, 3]))
    );
    runtime.shutdown().expect("clean shutdown");
}

/// A nested field addresses through its parent, which is what makes the schema's
/// key round-trip requirement load-bearing.
#[test]
fn a_nested_field_addresses_through_its_parent() {
    let (runtime, store) = spawn(AppState::sample());
    let zoom = store.field::<f64>("editor.zoom").expect("legal");
    assert_eq!(zoom.get().expect("store is up"), Reading::Present(1.0));
    zoom.set(&2.5).expect("writing through an existing parent");

    let editor = store.optional_field::<Editor>("editor").expect("legal");
    assert_eq!(
        editor.get().expect("store is up"),
        Reading::Present(Editor {
            zoom: 2.5,
            theme: "dark".to_string(),
        })
    );
    runtime.shutdown().expect("clean shutdown");
}
