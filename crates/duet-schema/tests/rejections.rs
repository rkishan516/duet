//! Mechanical proof that every refused type genuinely has no
//! [`SharedState`](duet_schema::SharedState) impl.
//!
//! # Why this file exists rather than a comment
//!
//! Duet refuses a type by *not implementing a trait for it*. That is the right
//! mechanism — see the crate docs — but it has one failure mode a table in a
//! README cannot catch: a blanket impl added later can quietly make a refused
//! type acceptable again. `impl<T: SharedState> SharedState for Vec<T>` is
//! exactly the shape that would do it, and a rejection that silently starts
//! compiling is worse than no rejection at all, because the documentation now
//! lies.
//!
//! So every row of the crate's rejection table is asserted here, mechanically,
//! against real trait resolution.
//!
//! # How the check works
//!
//! [`implements!`] resolves a trait bound to a `bool` **at compile time**, using
//! the fact that an *inherent* associated const shadows a trait one. `Wrapper<T>`
//! gets an inherent `IMPLS = true` only when `T` satisfies the bound; when it
//! does not, name resolution falls through to the blanket trait impl's
//! `IMPLS = false`. Nothing here inspects source text, so it cannot be fooled by
//! a type alias the way a macro's token inspection could be: `type Blob =
//! Vec<u8>;` resolves to `Vec<u8>` before the bound is ever tested.
//!
//! `trybuild` compile-fail cases — which additionally pin the *diagnostic text*
//! each rejection produces — live in `crates/duet-derive/tests/ui/`, alongside
//! the derive that most users will meet them through. **No `trybuild`
//! dev-dependency is added here**; this file needs none, and the crate's
//! dependency list stays as short as the manifest says.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsString;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime};

use duet_core::Value;
use duet_schema::{Bytes, NotNullable, SharedState};

/// Resolves `<type>: <bound>` to a compile-time `bool`.
macro_rules! implements {
    ($ty:ty : $($bound:tt)+) => {{
        #[allow(dead_code)]
        trait DoesNotImpl {
            const IMPLS: bool = false;
        }
        impl<T: ?Sized> DoesNotImpl for T {}

        struct Wrapper<T: ?Sized>(::core::marker::PhantomData<T>);

        #[allow(dead_code)]
        impl<T: ?Sized + $($bound)+> Wrapper<T> {
            const IMPLS: bool = true;
        }

        <Wrapper<$ty>>::IMPLS
    }};
}

/// Asserts the harness itself can tell the two answers apart.
///
/// Without this, a macro that always answered `false` would "verify" every
/// rejection below while proving nothing whatsoever. This is the control.
#[test]
fn the_harness_distinguishes_an_impl_from_its_absence() {
    assert!(implements!(i64: SharedState), "i64 is accepted");
    assert!(!implements!(u64: SharedState), "u64 is refused");
    assert!(implements!(Vec<i64>: SharedState), "Vec<i64> is accepted");
    assert!(!implements!(Vec<u64>: SharedState), "Vec<u64> is refused");
}

#[test]
fn every_accepted_type_really_is_accepted() {
    // The other half of the control: a change that broke acceptance would
    // otherwise leave every rejection test below still passing.
    assert!(implements!(bool: SharedState));
    assert!(implements!(i8: SharedState));
    assert!(implements!(i16: SharedState));
    assert!(implements!(i32: SharedState));
    assert!(implements!(i64: SharedState));
    assert!(implements!(u8: SharedState));
    assert!(implements!(u16: SharedState));
    assert!(implements!(u32: SharedState));
    assert!(implements!(f64: SharedState));
    assert!(implements!(String: SharedState));
    assert!(implements!(Bytes: SharedState));
    assert!(implements!(Value: SharedState));
    assert!(implements!(Option<i64>: SharedState));
    assert!(implements!(Vec<i64>: SharedState));
    assert!(
        implements!(Vec<u8>: SharedState),
        "accepted, as a list of ints"
    );
    assert!(implements!(HashMap<String, i64>: SharedState));
    assert!(implements!(BTreeMap<String, i64>: SharedState));
    assert!(implements!(Box<i64>: SharedState));
    assert!(implements!(Arc<i64>: SharedState));
    assert!(implements!(Vec<Option<Vec<String>>>: SharedState));
}

// --- Row 1: integers with no representation in `Value::Int` ---

#[test]
fn integers_that_do_not_fit_an_i64_are_refused() {
    // `u64::MAX` is not merely awkward to encode; above `i64::MAX` there is no
    // `Value::Int` that holds it at all. `usize`/`isize` are platform-width, so
    // a schema containing one would describe a different type on a 32-bit
    // target than on a 64-bit one.
    assert!(!implements!(u64: SharedState));
    assert!(!implements!(u128: SharedState));
    assert!(!implements!(i128: SharedState));
    assert!(!implements!(usize: SharedState));
    assert!(!implements!(isize: SharedState));
}

// --- Row 2: f32 ---

#[test]
fn f32_is_refused() {
    // Lossless outbound, lossy inbound — and Dart and TypeScript have no
    // 32-bit float, so narrowing here would buy nothing across the boundary
    // while costing precision on the way back.
    assert!(!implements!(f32: SharedState));
}

// --- Row 3: HashSet ---

#[test]
fn hash_sets_are_refused() {
    // Iteration order is not a function of the value, so `to_value` would not
    // be either: two equal sets would encode to two different byte strings,
    // breaking change detection and every golden file.
    assert!(!implements!(HashSet<i64>: SharedState));
    assert!(!implements!(HashSet<String>: SharedState));
}

// --- Row 4: maps that are not string-keyed ---

#[test]
fn maps_with_non_string_keys_are_refused() {
    // `Value::Map` is keyed by `String`. Lowering an integer-keyed map to a
    // list of pairs would make every path into it unaddressable.
    assert!(!implements!(HashMap<i64, i64>: SharedState));
    assert!(!implements!(BTreeMap<i64, i64>: SharedState));
    assert!(!implements!(HashMap<u8, String>: SharedState));
    assert!(!implements!(BTreeMap<Vec<u8>, String>: SharedState));
}

// --- Row 5: borrowed types ---

#[test]
fn borrowed_types_are_refused() {
    // The store owns a `'static` tree; a reference into it cannot survive the
    // core thread's next write, let alone a trip over IPC.
    assert!(!implements!(&str: SharedState));
    assert!(!implements!(&String: SharedState));
    assert!(!implements!(&[i64]: SharedState));
    assert!(!implements!(&i64: SharedState));
    assert!(!implements!(&mut i64: SharedState));
    assert!(!implements!(str: SharedState));
    assert!(!implements!([i64]: SharedState));
    assert!(!implements!(std::borrow::Cow<'static, str>: SharedState));
}

// --- Row 6: shared and interior-mutable handles ---

#[test]
fn shared_handles_and_interior_mutability_are_refused() {
    // Two handles to one node become two independent copies the moment they are
    // values in the tree, so the sharing an application wrote them for is
    // silently gone. `Rc` additionally is not `Send` and could not reach the
    // core thread. (`Arc<T>` *is* accepted, transparently — it loses its
    // sharing too, but it loses it visibly, at a type the store can hold.)
    assert!(!implements!(Rc<i64>: SharedState));
    assert!(!implements!(RefCell<i64>: SharedState));
    assert!(!implements!(Cell<i64>: SharedState));
    assert!(!implements!(Mutex<i64>: SharedState));
    assert!(!implements!(RwLock<i64>: SharedState));
    assert!(
        !implements!(Arc<Mutex<i64>>: SharedState),
        "the usual spelling"
    );
}

// --- Row 7: platform string types ---

#[test]
fn platform_string_types_are_refused() {
    // `OsString` is WTF-8 on Windows and can hold an unpaired surrogate;
    // `Value::Str` is UTF-8 only, and the wire rejects surrogates on both
    // encode and decode.
    assert!(!implements!(PathBuf: SharedState));
    assert!(!implements!(OsString: SharedState));
    assert!(!implements!(std::path::Path: SharedState));
    assert!(!implements!(std::ffi::OsStr: SharedState));
}

// --- Row 8: time ---

#[test]
fn time_types_are_refused() {
    // Whole seconds? Milliseconds? An RFC 3339 string? Every answer is
    // defensible and they disagree, so choosing one silently would be a
    // decision made on the developer's behalf that they cannot see.
    assert!(!implements!(Duration: SharedState));
    assert!(!implements!(SystemTime: SharedState));
    assert!(!implements!(Instant: SharedState));
}

// --- Row 9: nested options ---

#[test]
fn nested_options_are_refused() {
    // `Some(None)` and `None` both lower to `Value::Null`. The collapse is
    // unrepresentable rather than merely unimplemented: the store has no third
    // spelling to spend on it, and no `remove` either.
    assert!(!implements!(Option<Option<i64>>: SharedState));
    assert!(!implements!(Option<Option<String>>: SharedState));
    assert!(!implements!(Option<Box<Option<i64>>>: SharedState));
    assert!(!implements!(Option<Arc<Option<i64>>>: SharedState));
    // Deeper nesting must not sneak past by being deeper.
    assert!(!implements!(Option<Option<Option<i64>>>: SharedState));
}

#[test]
fn an_option_of_dynamic_is_refused_for_the_same_reason() {
    // A `Value` may itself *be* `Value::Null`, so `Some(Value::Null)` and
    // `None` collide exactly as a nested option does. This one is not on the
    // original rejection list; it falls out of the same rule, and it would
    // otherwise be the one hole in it.
    assert!(
        implements!(Value: SharedState),
        "dynamic itself is accepted"
    );
    assert!(!implements!(Option<Value>: SharedState));
    assert!(!implements!(Value: NotNullable));
}

// --- Rejections must not be bypassable by wrapping ---

#[test]
fn a_refused_type_stays_refused_inside_every_container() {
    // The real risk this file guards: a blanket impl making a refused type
    // acceptable again once it is nested.
    assert!(!implements!(Vec<u64>: SharedState));
    assert!(!implements!(Vec<f32>: SharedState));
    assert!(!implements!(Vec<Vec<usize>>: SharedState));
    assert!(!implements!(Option<u64>: SharedState));
    assert!(!implements!(Box<f32>: SharedState));
    assert!(!implements!(Arc<PathBuf>: SharedState));
    assert!(!implements!(HashMap<String, u64>: SharedState));
    assert!(!implements!(BTreeMap<String, HashSet<i64>>: SharedState));
    assert!(!implements!(Vec<HashMap<i64, i64>>: SharedState));
    assert!(!implements!(Option<Vec<Duration>>: SharedState));
}

#[test]
fn a_type_alias_does_not_defeat_the_rejection() {
    // The case that would defeat any macro-time token inspection: the derive
    // sees the alias, never the type it resolves to. Trait resolution runs
    // after type resolution, so the alias is transparent here.
    type Sneaky = u64;
    type AlsoSneaky = Option<Option<i64>>;
    type Blob = Vec<u8>;

    assert!(!implements!(Sneaky: SharedState));
    assert!(!implements!(AlsoSneaky: SharedState));
    // And the acceptance an alias could equally have hidden: `Blob` is a list
    // of integers, exactly as `Vec<u8>` is, with no special case anywhere.
    assert!(implements!(Blob: SharedState));
    assert_eq!(
        Blob::from(vec![1u8]).to_value(),
        Value::List(vec![Value::Int(1)])
    );
}

// --- The `NotNullable` marker, which the nested-option rejection rests on ---

#[test]
fn not_nullable_holds_for_everything_that_cannot_lower_to_null() {
    assert!(implements!(i64: NotNullable));
    assert!(implements!(bool: NotNullable));
    assert!(implements!(f64: NotNullable));
    assert!(implements!(String: NotNullable));
    assert!(implements!(Bytes: NotNullable));
    assert!(implements!(Vec<i64>: NotNullable));
    assert!(
        implements!(Vec<Option<i64>>: NotNullable),
        "a list is never null"
    );
    assert!(implements!(HashMap<String, i64>: NotNullable));
    assert!(implements!(BTreeMap<String, i64>: NotNullable));
    assert!(implements!(Box<i64>: NotNullable));
    assert!(implements!(Arc<i64>: NotNullable));
}

#[test]
fn not_nullable_fails_for_everything_that_can() {
    assert!(!implements!(Option<i64>: NotNullable));
    assert!(!implements!(Value: NotNullable));
    assert!(
        !implements!(Box<Option<i64>>: NotNullable),
        "a pointer must not erase its target's nullability"
    );
    assert!(!implements!(Arc<Option<i64>>: NotNullable));
}
