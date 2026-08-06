//! Turning a file path into the `package:` URI the compiler expects.
//!
//! `frontend_server` accepts either a `file://` path or a `package:` URI for
//! the entrypoint and for invalidated files. `flutter_tools` always sends
//! `package:` (`compile.dart`: `request.packageConfig.toPackageUri(...)`), and
//! this does the same.
//!
//! It matters more than a style preference. A library's identity inside a
//! compiled kernel is its URI, and the running program's kernel was built by
//! `flutter build`, which used `package:` URIs. Handing the incremental
//! compiler a `file://` URI for the *same* library invites the VM to treat the
//! diff as introducing a new library rather than replacing the existing one —
//! and a "successful" reload that changes nothing is a far worse failure than
//! a loud one, because the developer sees their edit silently not take effect.
//!
//! The mapping comes from `.dart_tool/package_config.json`, which `flutter pub
//! get` writes, so path dependencies and the project's own package are all
//! covered by the same lookup.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::error::{DevError, Stage};
use crate::frontend_server::file_uri;

/// One package's name and the absolute directory its `package:` URIs are
/// relative to.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Mapping {
    name: String,
    lib: PathBuf,
}

/// The package map from a `.dart_tool/package_config.json`.
#[derive(Debug, Clone, Default)]
pub struct PackageConfig {
    mappings: Vec<Mapping>,
}

impl PackageConfig {
    /// Reads and parses a `package_config.json`.
    ///
    /// Every `rootUri` in that file is relative to the file's own directory
    /// (they are written as `../`), so resolution is done against that rather
    /// than the process's working directory — which `cargo test` does not
    /// promise and a `duet dev` invoked from a subdirectory would get wrong.
    ///
    /// # Errors
    ///
    /// [`DevError::Io`] if the file cannot be read, [`DevError::NotFound`] if
    /// it is not the expected JSON shape.
    pub fn read(path: impl AsRef<Path>) -> Result<Self, DevError> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|source| DevError::Io {
            stage: Stage::LocateSdk,
            doing: "reading package_config.json",
            source,
        })?;
        let json: Value = serde_json::from_str(&text).map_err(|_| DevError::NotFound {
            stage: Stage::LocateSdk,
            what: "valid JSON in package_config.json",
            path: path.display().to_string(),
        })?;
        let base = path.parent().unwrap_or(Path::new("."));
        Ok(PackageConfig {
            mappings: read_mappings(&json, base),
        })
    }

    /// The `package:` URI for `file`, or a `file://` URI if no package covers
    /// it.
    ///
    /// The longest matching `lib` directory wins, so a package nested inside
    /// another package's tree resolves to the inner one.
    pub fn uri_for(&self, file: &Path) -> String {
        let file = normalise(file);
        let best = self
            .mappings
            .iter()
            .filter_map(|m| {
                file.strip_prefix(&m.lib)
                    .ok()
                    .map(|rest| (m.lib.components().count(), m, rest))
            })
            .max_by_key(|(depth, _, _)| *depth);

        match best {
            Some((_, mapping, rest)) => {
                // `package:` URIs always use forward slashes, whatever the
                // platform's separator is.
                let rest: Vec<String> = rest
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect();
                format!("package:{}/{}", mapping.name, rest.join("/"))
            }
            None => file_uri(&file),
        }
    }

    /// How many packages were mapped. For a startup diagnostic — an empty map
    /// means every URI will fall back to `file://`, which is worth seeing.
    pub fn len(&self) -> usize {
        self.mappings.len()
    }

    /// Whether no packages were mapped.
    pub fn is_empty(&self) -> bool {
        self.mappings.is_empty()
    }
}

/// Extracts `(name, absolute lib dir)` for every well-formed entry.
///
/// Skips malformed entries rather than failing the whole read: a
/// `package_config.json` with one odd entry should not stop a reload session,
/// and the consequence of skipping one is a `file://` fallback for files in
/// that package, which still compiles.
fn read_mappings(json: &Value, base: &Path) -> Vec<Mapping> {
    let Some(packages) = json.get("packages").and_then(Value::as_array) else {
        return Vec::new();
    };
    packages
        .iter()
        .filter_map(|entry| {
            let name = entry.get("name")?.as_str()?;
            let root = entry.get("rootUri")?.as_str()?;
            // `packageUri` is optional and defaults to the root itself.
            let package_uri = entry
                .get("packageUri")
                .and_then(Value::as_str)
                .unwrap_or("");
            let root = resolve(base, root);
            Some(Mapping {
                name: name.to_string(),
                lib: normalise(&root.join(package_uri)),
            })
        })
        .collect()
}

/// Resolves a `rootUri` — which may be relative, or a `file://` URI — against
/// the directory holding `package_config.json`.
fn resolve(base: &Path, root_uri: &str) -> PathBuf {
    match root_uri.strip_prefix("file://") {
        Some(absolute) => PathBuf::from(absolute),
        None => base.join(root_uri),
    }
}

/// Removes `.` and `..` components so prefix comparison works.
///
/// Lexical, not `canonicalize`: `rootUri` is written as `../`, so the joined
/// path is full of `..` components that would defeat `strip_prefix` even
/// though the paths denote the same directory. Deliberately does *not* touch
/// the filesystem — `canonicalize` would also resolve symlinks, and a project
/// reached through one would then stop matching the paths the watcher reports.
fn normalise(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                // Popping past the root is not possible for an absolute path
                // and harmless for a relative one.
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
#[path = "packages_tests.rs"]
mod tests;
