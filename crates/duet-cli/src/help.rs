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
    duet dev --flutter <dir> [--flutter-root <dir>] -- <host command>
    duet --help
    duet --version

COMMANDS
    generate    Emit a typed Dart and/or TypeScript client from a schema.
                Run `duet generate --help` for its flags.
    dev         Run the host and hot-reload its Dart guest whenever a file
                under lib/ is saved. Run `duet dev --help` for its flags.

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

/// `duet dev --help`.
pub const DEV_HELP: &str = "\
duet dev — run the host, and hot-reload its Dart guest on save.

Starts the host, watches the Flutter project's lib/ directory, and on every
save recompiles just what changed and applies it to the running Dart isolate.
State is not lost: this is hot reload, not restart. The Rust store keeps its
contents because the host is never restarted, and the Dart heap keeps its
because `reloadSources` patches the isolate in place.

USAGE
    duet dev --flutter <dir> [--flutter-root <dir>] [--entrypoint <uri>] \\
        -- <host command>

FLAGS
    --flutter <dir>        The Flutter project: the directory holding
                           pubspec.yaml and .dart_tool/. Required.
    --flutter-root <dir>   The Flutter SDK checkout. Defaults to $FLUTTER_ROOT.
    --entrypoint <uri>     The Dart entrypoint, as a package: URI. Defaults to
                           the project's own package plus lib/main.dart.
    -h, --help             Print this.

Everything after `--` is the command that runs the host, passed through
unsplit. `dev` starts the host itself rather than attaching to a running one,
because that is how it reads the Dart VM service URI: the engine prints it to
stdout from native code, and a child process's stdout is an ordinary pipe.
Nothing is redirected, and the VM service keeps its authentication code.

EXAMPLES
    A Duet host built by cargo:
        duet dev --flutter ./flutter -- cargo run -p my-host

    Passing flags on to the host command:
        duet dev --flutter ./flutter -- cargo run -p my-host --features dev

REQUIREMENTS
    The host must be a debug/JIT build. A release/AOT engine has no Dart VM
    service, so there is nothing to reload through. `flutter pub get` must have
    run in the project, which is what writes the package map the incremental
    compiler needs.

A change hot reload cannot express — a class's shape, an enum's values, a const
class's fields — is reported as such, and needs the host restarted. A Dart file
that does not compile is reported with the compiler's own diagnostics and the
host keeps running.
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
        for flag in crate::args::DEV_FLAGS {
            assert!(DEV_HELP.contains(flag), "{flag} is undocumented");
        }
        assert!(HELP.contains("duet generate --schema"));
        assert!(HELP.contains("--help"));
        assert!(HELP.contains("--version"));
    }

    #[test]
    fn the_top_level_help_names_every_command_the_parser_dispatches() {
        // A command that exists and is not listed is one nobody discovers.
        for command in ["generate", "dev"] {
            assert!(
                HELP.contains(&format!("    {command}    ")),
                "the `{command}` command is missing from COMMANDS"
            );
        }
    }

    #[test]
    fn the_dev_help_says_the_host_must_be_a_debug_build() {
        // The single most likely way for `duet dev` to fail for a new user: a
        // release/AOT host has no VM service at all, and the failure at that
        // point is a timeout with no obvious cause.
        assert!(
            DEV_HELP.contains("JIT"),
            "the JIT requirement is undocumented"
        );
        assert!(
            DEV_HELP.contains("pub get"),
            "the pub get requirement is undocumented"
        );
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
        for text in [HELP, GENERATE_HELP, DEV_HELP, &version()] {
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
