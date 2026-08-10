//! Locating the three Flutter SDK artefacts a hot reload needs.
//!
//! `frontend_server` is not on anybody's `PATH`. It ships as an AOT snapshot
//! inside a Flutter checkout and is run by the Dart SDK's own AOT runtime,
//! against the *patched* Dart SDK that Flutter builds. Getting any of those
//! three wrong produces failures that look nothing like their cause: a missing
//! trailing slash on `--sdk-root` makes the compiler silently misbehave rather
//! than complain, and a mismatched patched SDK produces kernel the VM refuses.
//!
//! So this resolves all three from one Flutter root, checks each exists, and
//! names the missing one. That check is the entire value of this module: it
//! converts "the reload hung" into "no frontend_server snapshot at `<path>`".

use std::path::{Path, PathBuf};

use crate::error::{DevError, Stage};

/// The platform's file name for the Dart AOT runtime.
///
/// The Dart SDK ships it as `dartaotruntime.exe` on Windows and
/// `dartaotruntime` everywhere else; looking for the bare name on Windows can
/// never resolve, which left [`FlutterSdk::locate`] unable to find a real,
/// fully-populated SDK there (found by the Windows `hot_reload` example).
///
/// Public so that anything constructing an SDK-shaped directory — test
/// fixtures most of all — names the file the way a real checkout does; a fake
/// built with the bare name on Windows would pass against a resolver that
/// could never find the real thing.
pub const DART_AOT_RUNTIME: &str = if cfg!(windows) {
    "dartaotruntime.exe"
} else {
    "dartaotruntime"
};

/// The Flutter SDK artefacts needed to run an incremental compiler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlutterSdk {
    /// The Dart AOT runtime that executes the snapshot.
    pub dartaotruntime: PathBuf,
    /// The `frontend_server` AOT snapshot.
    pub frontend_server: PathBuf,
    /// Flutter's patched Dart SDK, which the compiler compiles against.
    ///
    /// Held as a [`PathBuf`]; [`FlutterSdk::sdk_root_argument`] is what adds
    /// the trailing slash the compiler needs.
    pub patched_sdk: PathBuf,
}

impl FlutterSdk {
    /// Resolves the three artefacts under `flutter_root` and checks each one
    /// is there.
    ///
    /// # Errors
    ///
    /// [`DevError::NotFound`] naming the first artefact that is missing,
    /// including `flutter_root` itself — a typo in the root would otherwise
    /// produce three confusing errors instead of one obvious one.
    pub fn locate(flutter_root: impl AsRef<Path>) -> Result<Self, DevError> {
        let root = flutter_root.as_ref();
        require(root, "Flutter SDK directory")?;

        let cache = root.join("bin").join("cache");
        let sdk = FlutterSdk {
            dartaotruntime: cache.join("dart-sdk").join("bin").join(DART_AOT_RUNTIME),
            frontend_server: cache
                .join("dart-sdk")
                .join("bin")
                .join("snapshots")
                .join("frontend_server_aot.dart.snapshot"),
            patched_sdk: cache
                .join("artifacts")
                .join("engine")
                .join("common")
                .join("flutter_patched_sdk"),
        };
        require(&sdk.dartaotruntime, "dartaotruntime")?;
        require(&sdk.frontend_server, "frontend_server snapshot")?;
        require(&sdk.patched_sdk, "flutter_patched_sdk directory")?;
        Ok(sdk)
    }

    /// The value for `--sdk-root`, **with a trailing separator**.
    ///
    /// Not cosmetic. Spike C recorded that `frontend_server` "silently
    /// misbehaves without it" — it treats the value as a prefix rather than a
    /// directory, so `…/flutter_patched_sdk` resolves core libraries against
    /// `…/flutter_patched_sdklib/…`, which does not exist, and the failure
    /// surfaces much later as unresolved `dart:core`.
    pub fn sdk_root_argument(&self) -> String {
        let mut text = self.patched_sdk.display().to_string();
        if !text.ends_with(std::path::MAIN_SEPARATOR) {
            text.push(std::path::MAIN_SEPARATOR);
        }
        text
    }
}

/// Errors with the path if it is not there.
fn require(path: &Path, what: &'static str) -> Result<(), DevError> {
    if path.exists() {
        return Ok(());
    }
    Err(DevError::NotFound {
        stage: Stage::LocateSdk,
        what,
        path: path.display().to_string(),
    })
}

/// A Flutter project's `.dart_tool/package_config.json`.
///
/// Separate from [`FlutterSdk`] because it belongs to the *project*, not the
/// toolchain, and because its absence has a different fix: the SDK is missing
/// if Flutter is not installed, but this is missing if `flutter pub get` has
/// not been run.
///
/// # Errors
///
/// [`DevError::NotFound`] if the project directory or the package config is
/// missing.
pub fn package_config(project: impl AsRef<Path>) -> Result<PathBuf, DevError> {
    let project = project.as_ref();
    require(project, "Flutter project directory")?;
    let config = project.join(".dart_tool").join("package_config.json");
    require(&config, "package_config.json (has `flutter pub get` run?)")?;
    Ok(config)
}

#[cfg(test)]
#[path = "sdk_tests.rs"]
mod tests;
