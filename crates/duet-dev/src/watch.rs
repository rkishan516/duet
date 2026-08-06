//! A debounced, polling file watcher.
//!
//! # Why polling, and when that stops being right
//!
//! `notify` is the obvious dependency, and it wraps three genuinely different
//! OS mechanisms (FSEvents, inotify, `ReadDirectoryChangesW`) with three
//! genuinely different sets of quirks: FSEvents reports directory-granular
//! events with a coalescing latency, inotify needs one watch descriptor per
//! directory and silently stops at a per-user limit, and all three can drop
//! events under load and leave a watcher permanently stale.
//!
//! Polling `stat` has none of that. It cannot miss a change, it behaves
//! identically on every platform, and its state is a map this crate owns and
//! can test with an injected clock. The cost is `O(files)` `stat` calls per
//! tick: a Flutter project's `lib/` is typically tens to low hundreds of files,
//! a `stat` is a couple of microseconds, so a 250 ms tick over 500 files is
//! about a millisecond of work every quarter second. That is not a trade worth
//! a dependency and a platform matrix.
//!
//! **Where this stops being right:** watching a tree with tens of thousands of
//! files, or wanting sub-10 ms change detection. Neither describes a Dart
//! source tree — and note this deliberately does not descend into
//! `.dart_tool/` or `build/`, which is where a Flutter project's file count
//! actually lives.
//!
//! # Debounce is trailing-edge
//!
//! An editor saving a file often produces several filesystem operations, and
//! "format on save" produces a second write milliseconds after the first.
//! Recompiling on the first one means compiling a half-written file. So a
//! batch is released only after [`WatchConfig::debounce`] of quiet, and every
//! change seen during the wait extends it. The batch that comes out is the
//! union of everything that changed, which is exactly the invalidated-file
//! list `recompile` wants.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use crate::error::{DevError, Stage};

/// Directory names never descended into.
///
/// `.dart_tool` and `build` hold generated output — including the very dill
/// this crate writes — so watching them would make the driver retrigger on its
/// own compiler's output, forever. The rest are ordinary noise.
const SKIPPED_DIRECTORIES: [&str; 6] = [".dart_tool", "build", ".git", ".idea", ".dart", "ios"];

/// How deep to descend before giving up.
///
/// A guard against a symlink cycle, which `read_dir` will happily follow until
/// the process runs out of stack or patience. Real Dart trees are nowhere near
/// this deep.
const MAX_DEPTH: usize = 24;

/// What a watcher looks at.
#[derive(Debug, Clone)]
pub struct WatchConfig {
    /// Directories to scan, recursively.
    pub roots: Vec<PathBuf>,
    /// File extensions that count as a change, without the dot.
    pub extensions: Vec<String>,
    /// How long the tree must be quiet before a batch is released.
    pub debounce: Duration,
}

impl WatchConfig {
    /// Watches `lib/` under a Flutter project for `.dart` files, with a 120 ms
    /// quiet period.
    ///
    /// 120 ms is comfortably longer than the gap between an editor's write and
    /// its format-on-save rewrite, and small enough to stay invisible next to
    /// the ~123 ms Spike C measured for the reload itself.
    pub fn dart_project(project: impl AsRef<Path>) -> Self {
        WatchConfig {
            roots: vec![project.as_ref().join("lib")],
            extensions: vec!["dart".to_string()],
            debounce: Duration::from_millis(120),
        }
    }
}

/// One file's identity for change detection.
///
/// Size as well as mtime. Filesystems vary in mtime resolution, and a rewrite
/// that lands within the same tick as the previous one — a formatter running
/// immediately after a save — can carry an identical timestamp. Comparing the
/// length too catches the overwhelmingly common case of that edit also
/// changing the file's size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    modified: Option<SystemTime>,
    len: u64,
}

/// A polling watcher over a fixed set of roots.
pub struct Watcher {
    config: WatchConfig,
    /// Every watched file and the stamp last seen for it.
    seen: BTreeMap<PathBuf, Stamp>,
    /// Files changed since the last released batch.
    pending: BTreeSet<PathBuf>,
    /// When the most recent change was observed, for the quiet period.
    last_change: Option<Instant>,
}

impl Watcher {
    /// Builds a watcher and takes the baseline scan.
    ///
    /// The baseline is why the first [`Watcher::poll`] reports nothing: every
    /// file already present is *known*, not *changed*. A watcher that reported
    /// the whole tree on its first tick would trigger a full recompile every
    /// time `duet dev` started.
    ///
    /// # Errors
    ///
    /// [`DevError::NotFound`] if a root does not exist — a mistyped project
    /// path should fail at startup, not silently watch nothing forever.
    pub fn new(config: WatchConfig) -> Result<Self, DevError> {
        for root in &config.roots {
            if !root.exists() {
                return Err(DevError::NotFound {
                    stage: Stage::Watch,
                    what: "directory to watch",
                    path: root.display().to_string(),
                });
            }
        }
        let mut watcher = Watcher {
            config,
            seen: BTreeMap::new(),
            pending: BTreeSet::new(),
            last_change: None,
        };
        watcher.seen = watcher.scan();
        Ok(watcher)
    }

    /// Rescans, and returns a batch once the tree has been quiet for
    /// [`WatchConfig::debounce`].
    ///
    /// `now` is passed in rather than read, so the debounce is testable
    /// without sleeping. Returns `None` while nothing has changed, and also
    /// while changes are still arriving.
    pub fn poll(&mut self, now: Instant) -> Option<Vec<PathBuf>> {
        let current = self.scan();
        let changed = differences(&self.seen, &current);
        if !changed.is_empty() {
            self.pending.extend(changed);
            self.last_change = Some(now);
            self.seen = current;
        }

        let last = self.last_change?;
        if now.duration_since(last) < self.config.debounce {
            return None;
        }
        self.last_change = None;
        let batch: Vec<PathBuf> = std::mem::take(&mut self.pending).into_iter().collect();
        (!batch.is_empty()).then_some(batch)
    }

    /// How many files are being watched. For the startup line `duet dev`
    /// prints, so "watching 0 files" is visible rather than mysterious.
    pub fn watched(&self) -> usize {
        self.seen.len()
    }

    /// Stamps every matching file under every root.
    ///
    /// Infallible on purpose. A directory that vanishes mid-scan, or one this
    /// process cannot read, is not a reason to stop watching the rest — and
    /// the file simply disappearing from the map is already reported as a
    /// change, which is exactly right.
    fn scan(&self) -> BTreeMap<PathBuf, Stamp> {
        let mut out = BTreeMap::new();
        for root in &self.config.roots {
            self.walk(root, 0, &mut out);
        }
        out
    }

    /// One directory, recursively, bounded by [`MAX_DEPTH`].
    fn walk(&self, directory: &Path, depth: usize, out: &mut BTreeMap<PathBuf, Stamp>) {
        if depth > MAX_DEPTH {
            return;
        }
        let Ok(entries) = std::fs::read_dir(directory) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                if !is_skipped(&path) {
                    self.walk(&path, depth + 1, out);
                }
                continue;
            }
            if !self.matches(&path) {
                continue;
            }
            let stamp = entry.metadata().map_or(
                Stamp {
                    modified: None,
                    len: 0,
                },
                |m| Stamp {
                    modified: m.modified().ok(),
                    len: m.len(),
                },
            );
            out.insert(path, stamp);
        }
    }

    /// Whether a file's extension is one being watched.
    fn matches(&self, path: &Path) -> bool {
        let Some(extension) = path.extension().and_then(|e| e.to_str()) else {
            return false;
        };
        self.config
            .extensions
            .iter()
            .any(|wanted| wanted.eq_ignore_ascii_case(extension))
    }
}

/// Whether a directory should not be descended into.
///
/// Matches [`SKIPPED_DIRECTORIES`] and anything starting with `.`, which
/// covers editor and tool state without needing to enumerate it.
fn is_skipped(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return true;
    };
    name.starts_with('.') || SKIPPED_DIRECTORIES.contains(&name)
}

/// Paths that were added, removed, or whose stamp moved.
///
/// A free function over two maps so every case — creation, deletion,
/// modification, and a rename, which is a deletion plus a creation — is
/// testable without touching a filesystem.
fn differences(
    before: &BTreeMap<PathBuf, Stamp>,
    after: &BTreeMap<PathBuf, Stamp>,
) -> Vec<PathBuf> {
    let mut changed = Vec::new();
    for (path, stamp) in after {
        if before.get(path) != Some(stamp) {
            changed.push(path.clone());
        }
    }
    for path in before.keys() {
        if !after.contains_key(path) {
            changed.push(path.clone());
        }
    }
    changed.sort();
    changed.dedup();
    changed
}

#[cfg(test)]
#[path = "watch_tests.rs"]
mod tests;
