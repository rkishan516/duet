//! The text a newcomer meets first.
//!
//! Written rather than generated, and held here as constants so that
//! `tests/cli.rs` can assert the bytes a user sees. Help output is documentation
//! that ships in the binary: if it drifts from what the tool does, it is worse
//! than absent, because it is believed.

/// `duet --help`.
///
/// The whole framework in one screen: the shape of the pipeline, the two things
/// this command does, and the exit codes a script needs.
pub const HELP: &str = "\
duet — typed clients for a shared Rust store.

A Duet host owns the state. A Flutter engine and a webview are guests that read
and write it over one wire format. This command turns the schema in the middle
into typed client code, so no guest hand-writes a path string.

    #[derive(SharedState)]   ->   schema.json   ->   duet generate   ->   Dart
    on a Rust struct              the contract       this command          TypeScript

USAGE
    duet generate --schema <path> [--dart <path>] [--ts <path>] [--check]
    duet --help
    duet --version

COMMANDS
    generate    Emit a typed Dart and/or TypeScript client from a schema.
                Run `duet generate --help` for its flags.

EXIT CODES
    0    the files were written, or --check found them up to date
    1    the schema could not be read, or a file could not be written
    2    the command line was wrong
    3    --check found a file that differs from what would be generated

Getting a schema: a Rust type deriving `SharedState` renders one with
`duet::Schema::of::<App>()?.render()`. Or write the JSON by hand — the format is
the contract, and nothing about it assumes a Rust producer.
";

/// `duet generate --help`.
pub const GENERATE_HELP: &str = "\
duet generate — emit typed clients from a Duet schema.

Reads one schema document and writes a typed client for each language asked for.
Every path in the output is a string literal minted and checked at generation
time, so generated code never assembles a path from a runtime value.

USAGE
    duet generate --schema <path> [--dart <path>] [--ts <path>] [--check]

FLAGS
    --schema <path>    The schema document to read. Required.
    --dart <path>      Where to write the Dart client.
    --ts <path>        Where to write the TypeScript client.
    --check            Write nothing. Compare what is on disk against what
                       would be generated, print a diff for each file that
                       differs, and exit 3 if any does.
    -h, --help         Print this.

At least one of --dart and --ts is required: a run that generated nothing would
exit 0 and look exactly like a run that succeeded.

Missing parent directories are created. Each file is written to a temporary
name in its own directory and renamed into place only after every file has been
staged, so a failure part-way through leaves the previous contents intact.

EXAMPLES
    Generate both clients:
        duet generate --schema schema/app.json \\
            --dart lib/src/app.duet.dart --ts src/app.duet.ts

    Fail a build when a committed client is stale:
        duet generate --schema schema/app.json \\
            --dart lib/src/app.duet.dart --ts src/app.duet.ts --check

The TypeScript output imports the runtime from `duet-protocol` and
`duet-protocol/typed`; the Dart output imports `package:duet/duet.dart`. Both
are the published packages, so generated code compiles wherever they resolve.
";

/// `duet --version`.
///
/// The version is this crate's, which is the workspace's, which is the version
/// of the emitters that produced the output — the number a reader of a
/// generated file actually needs.
pub fn version() -> String {
    format!("duet {}\n", env!("CARGO_PKG_VERSION"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_help_texts_name_every_flag_the_parser_accepts() {
        // The one drift that matters: a flag the parser knows and the help does
        // not is a feature nobody can find, and the reverse is a documented lie.
        for flag in ["--schema", "--dart", "--ts", "--check"] {
            assert!(GENERATE_HELP.contains(flag), "{flag} is undocumented");
        }
        assert!(HELP.contains("duet generate --schema"));
        assert!(HELP.contains("--help"));
        assert!(HELP.contains("--version"));
    }

    #[test]
    fn the_top_level_help_explains_every_exit_code_the_binary_can_return() {
        for code in ["0", "1", "2", "3"] {
            assert!(
                HELP.contains(&format!("    {code}    ")),
                "exit code {code} is undocumented"
            );
        }
    }

    #[test]
    fn every_help_text_ends_with_exactly_one_newline() {
        for text in [HELP, GENERATE_HELP, &version()] {
            assert!(text.ends_with('\n'), "{text:?}");
            assert!(!text.ends_with("\n\n"), "{text:?}");
        }
    }

    #[test]
    fn the_version_is_the_crate_version() {
        assert_eq!(version(), format!("duet {}\n", env!("CARGO_PKG_VERSION")));
        assert!(version().starts_with("duet 0."));
    }
}
