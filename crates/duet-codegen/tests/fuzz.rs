//! The reader against arbitrary, truncated and mutated input.
//!
//! A schema file is an input like any other: read from disk, possibly written
//! by a tool, possibly corrupted, possibly hostile. `read_schema` returns a
//! `Result` and the claim behind that signature is that **every** byte string
//! reaches one arm of it — no panic, no unbounded recursion, no unbounded
//! allocation.
//!
//! A hand-written table of bad documents cannot make that claim: it only covers
//! the ways its author thought of. These three generators reach the shapes a
//! table does not — a truncation at every single byte offset, a mutation at
//! every byte, and structures deep enough to overflow a stack.
//!
//! The generator is a deterministic pseudo-random one, seeded from a constant,
//! for the reason every fuzz test in this repository is: a failure has to be
//! reproducible from the test name alone.

use duet_codegen::{Options, generate, read_schema};

/// The valid document the mutations start from.
const SEED: &str = r#"{
  "root": {"kind": "named", "name": "App"},
  "types": [
    {
      "fields": [
        {"key": "counter", "type": {"kind": "int"}},
        {"key": "editor", "type": {"kind": "named", "name": "Editor"}},
        {"key": "tags", "type": {"kind": "list", "of": {"kind": "string"}}}
      ],
      "name": "App"
    },
    {
      "fields": [
        {"key": "zoom", "type": {"kind": "float"}}
      ],
      "name": "Editor"
    }
  ],
  "version": 1
}
"#;

#[test]
fn the_seed_document_is_actually_valid() {
    // Without this, every mutation below could be failing for the same reason
    // the seed does, and the fuzz would be measuring nothing.
    let schema = read_schema(SEED).expect("the seed should be a valid schema");
    generate(&schema, &Options::new("test", "test")).expect("the seed should be emittable");
}

#[test]
fn every_truncation_of_a_valid_document_is_survived() {
    // Every prefix, byte by byte: the shape a half-written file has, and the
    // one a reader that indexes past the end meets first.
    for end in 0..SEED.len() {
        if !SEED.is_char_boundary(end) {
            continue;
        }
        let _ = read_schema(&SEED[..end]);
    }
}

#[test]
fn every_single_byte_mutation_is_survived() {
    // One byte changed at a time, over the whole document, through a set of
    // replacements chosen to break structure rather than content: brackets,
    // quotes, separators and a non-ASCII byte.
    for at in 0..SEED.len() {
        for replacement in [b'{', b'}', b'[', b']', b'"', b':', b',', b'\\', b'0', 0xff] {
            let mut bytes = SEED.as_bytes().to_vec();
            bytes[at] = replacement;
            match String::from_utf8(bytes) {
                Ok(text) => {
                    let _ = read_schema(&text);
                }
                // A byte swap that breaks UTF-8 is a file `read_to_string`
                // would have refused before this crate saw it.
                Err(_) => continue,
            }
        }
    }
}

#[test]
fn arbitrary_bytes_are_survived() {
    let mut rng = Rng::new(0x5EED_D0CE);
    for _ in 0..4_000 {
        let length = rng.below(120);
        let text: String = (0..length).map(|_| rng.printable()).collect();
        let _ = read_schema(&text);
    }
}

#[test]
fn structures_deep_enough_to_overflow_a_stack_are_refused_rather_than_survived_by_luck() {
    // Three depths, each past a different limit:
    //
    // - 200 nested type constructors: past `MAX_TY_DEPTH`, inside `serde_json`'s
    //   own parse limit.
    // - 5_000: past `serde_json`'s default recursion limit, which is left on
    //   here precisely so this is a parse error rather than an abort.
    // - 100_000 unclosed brackets: not even a document, and the shape that
    //   overflows a recursive-descent parser with no limit at all.
    for depth in [200usize, 5_000] {
        let mut ty = "{\"kind\": \"int\"}".to_string();
        for _ in 0..depth {
            ty = format!("{{\"kind\": \"list\", \"of\": {ty}}}");
        }
        let document = format!(
            "{{\"root\": {{\"kind\": \"named\", \"name\": \"App\"}}, \"types\": \
             [{{\"fields\": [{{\"key\": \"deep\", \"type\": {ty}}}], \"name\": \"App\"}}], \
             \"version\": 1}}"
        );
        assert!(
            read_schema(&document).is_err(),
            "{depth} nested constructors should be refused"
        );
    }
    assert!(read_schema(&"[".repeat(100_000)).is_err());
    assert!(read_schema(&"{\"a\":".repeat(100_000)).is_err());
}

#[test]
fn a_document_full_of_repeated_keys_does_not_allocate_without_bound() {
    // `serde_json::Map` is a `BTreeMap` here, so a million repeats of one key
    // collapse to one entry rather than a million. Stated as a test because the
    // opposite — a `Vec` of pairs — would turn a small file into a large
    // allocation, and the choice is a feature flag away.
    let document = format!(
        "{{\"root\": {{{}\"kind\": \"int\"}}, \"types\": [], \"version\": 1}}",
        "\"kind\": \"bool\", ".repeat(50_000)
    );
    let schema = read_schema(&document).expect("repeats collapse to the last one");
    assert_eq!(
        schema.root(),
        &duet_schema::Ty::Int,
        "the last spelling of a repeated key wins"
    );
    // A scalar root is a valid schema and still not something a generated
    // client can be built from; the two rejections are different layers.
    assert!(generate(&schema, &Options::new("test", "test")).is_err());
}

/// A small deterministic generator, so a failure reproduces from the test name.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Rng {
        Rng(seed)
    }

    /// xorshift64*, chosen because it is four lines and needs no dependency.
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }

    /// A character from the alphabet a JSON document is made of, weighted
    /// towards the structural ones so the output is *nearly* JSON rather than
    /// noise a parser rejects on its first byte.
    fn printable(&mut self) -> char {
        const ALPHABET: &[u8] = b"{}[]\":,0123456789abcdefghijklmnopqrstuvwxyz \n\t-.";
        let index = (self.next() as usize) % ALPHABET.len();
        char::from(ALPHABET[index])
    }
}
