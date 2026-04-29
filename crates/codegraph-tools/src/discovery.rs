//! Parallel filesystem discovery walker.
//!
//! Wraps `ignore::WalkBuilder::build_parallel()` with the project's
//! [`DiscoveryConfig`] knobs and the [`LanguageRegistry`] extension filter,
//! emitting [`DiscoveredFile`] records over a `crossbeam_channel`. Walk
//! errors (permission denied, broken symlinks, glob failures) flow over a
//! sibling channel and surface as `String` warnings on the returned
//! [`Discovered`] struct.
//!
//! Filtering is in-thread: each worker checks `registry.language_for_path`
//! against the visited path and drops non-source files immediately, so the
//! result `Vec` never contains `.png`/`.o`/`node_modules/*.js` entries that
//! would have to be discarded post-walk.
//!
//! The walker is synchronous; the Phase 3.4 `analyze_codebase` handler will
//! call it from `tokio::task::spawn_blocking`.
//!
//! ## extra_ignore semantics
//!
//! The design's `add_ignore_path_from_pattern` method does not exist on
//! `ignore::WalkBuilder` 0.4. Instead, we feed
//! `DiscoveryConfig::extra_ignore` patterns through `OverrideBuilder` —
//! per the `ignore` docs, an `OverrideBuilder` glob with a `!` prefix
//! "ignores" matching files. We prepend `!` automatically so users write
//! positive patterns (e.g. `"target/**"`) in their `.code-graph.toml` and
//! get exclusion behavior without thinking about override-vs-ignore
//! semantics. Glob compile failures land in `Discovered.warnings` and the
//! offending pattern is skipped — one bad pattern does not abort discovery.

use std::path::{Path, PathBuf};

use codegraph_core::{DiscoveryConfig, Language};
use codegraph_lang::LanguageRegistry;

/// One source file located by the walker.
#[derive(Debug, Clone)]
pub struct DiscoveredFile {
    pub path: PathBuf,
    pub language: Language,
}

/// Aggregate result of a discovery pass.
#[derive(Debug, Default)]
pub struct Discovered {
    pub files: Vec<DiscoveredFile>,
    pub warnings: Vec<String>,
}

/// Walk `root` in parallel and return every file whose extension is claimed
/// by a plugin in `registry`.
///
/// The walker spawns `cfg.max_threads` workers (or `ignore`'s default
/// heuristic when `cfg.max_threads == 0`), filters by extension in-thread,
/// and collects results via `crossbeam_channel::unbounded`. Walk errors
/// (permission denied, glob failures, broken symlinks) become `String`
/// warnings on the returned [`Discovered`] rather than aborting the walk.
///
/// Results are sorted by path before returning so the binary output is
/// reproducible across runs and matches the Go binary's
/// `sort.Strings(files)` ordering byte-for-byte.
pub fn discover(root: &Path, registry: &LanguageRegistry, cfg: &DiscoveryConfig) -> Discovered {
    let (file_tx, file_rx) = crossbeam_channel::unbounded::<DiscoveredFile>();
    let (warn_tx, warn_rx) = crossbeam_channel::unbounded::<String>();

    let mut builder = ignore::WalkBuilder::new(root);
    builder
        .threads(cfg.max_threads)
        // standard_filters(true) enables hidden+parents+ignore+git_ignore+
        // git_global+git_exclude as a group; (false) disables them. We
        // override `hidden(false)` afterwards so dotfile directories are
        // still descended into unless gitignore says otherwise — matching
        // the Go binary's behavior.
        .standard_filters(cfg.respect_gitignore)
        .follow_links(cfg.follow_symlinks)
        .hidden(false)
        // By default, the `ignore` crate only honors `.gitignore` when a
        // `.git` directory is present. Users routinely point this binary
        // at subdirectories of a repo (or at non-git source trees that
        // still ship a `.gitignore`); disabling the require_git gate
        // keeps the contract intuitive.
        .require_git(false);

    // extra_ignore patterns: feed through OverrideBuilder with `!` prefix
    // (which inverts override→ignore semantics, per the ignore-crate docs).
    if !cfg.extra_ignore.is_empty() {
        let mut ob = ignore::overrides::OverrideBuilder::new(root);
        let mut had_globs = false;
        for pat in &cfg.extra_ignore {
            // If the user already wrote a `!`-prefixed pattern, pass it
            // through verbatim; otherwise prepend `!` to mark it as an
            // ignore.
            let pat = if pat.starts_with('!') {
                pat.clone()
            } else {
                format!("!{pat}")
            };
            match ob.add(&pat) {
                Ok(_) => {
                    had_globs = true;
                }
                Err(e) => {
                    let _ = warn_tx.send(format!("invalid extra_ignore pattern {pat:?}: {e}"));
                }
            }
        }
        if had_globs {
            match ob.build() {
                Ok(ov) => {
                    builder.overrides(ov);
                }
                Err(e) => {
                    let _ = warn_tx.send(format!("failed to build extra_ignore overrides: {e}"));
                }
            }
        }
    }

    builder.build_parallel().run(|| {
        let file_tx = file_tx.clone();
        let warn_tx = warn_tx.clone();
        Box::new(move |entry| {
            match entry {
                Ok(e) => {
                    if e.file_type().is_some_and(|ft| ft.is_file()) {
                        if let Some(lang) = registry.language_for_path(e.path()) {
                            let path = e.into_path();
                            let _ = file_tx.send(DiscoveredFile {
                                path,
                                language: lang,
                            });
                        }
                    }
                }
                Err(err) => {
                    let _ = warn_tx.send(format!("walk error: {err}"));
                }
            }
            ignore::WalkState::Continue
        })
    });

    // Drop the original senders so the receiver's try_iter() actually ends.
    // Each worker thread cloned its own sender; those clones drop when the
    // closure returns, leaving these as the last references.
    drop(file_tx);
    drop(warn_tx);

    let mut files: Vec<DiscoveredFile> = file_rx.into_iter().collect();
    let warnings: Vec<String> = warn_rx.into_iter().collect();

    // Sort for deterministic output. Required for the parse-test parity
    // gate — Go's `sort.Strings(files)` produces a global lexicographic
    // ordering, and the `ignore` parallel walker visits in arbitrary
    // worker order.
    files.sort_by(|a, b| a.path.cmp(&b.path));

    Discovered { files, warnings }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codegraph_core::{FileGraph, Language};
    use codegraph_lang::{LanguagePlugin, ParseError};
    use std::fs;
    use std::path::Path;
    use tempfile::TempDir;

    /// Test plugin claiming a fixed extension list. Reused by every
    /// fixture in this module.
    struct StubPlugin {
        id: Language,
        exts: &'static [&'static str],
    }

    impl LanguagePlugin for StubPlugin {
        fn id(&self) -> Language {
            self.id
        }
        fn extensions(&self) -> &'static [&'static str] {
            self.exts
        }
        fn parse_file(&self, path: &Path, _content: &[u8]) -> Result<FileGraph, ParseError> {
            Ok(FileGraph {
                path: path.to_string_lossy().into_owned(),
                language: self.id,
                symbols: Vec::new(),
                edges: Vec::new(),
            })
        }
    }

    fn cpp_only_registry() -> LanguageRegistry {
        let mut reg = LanguageRegistry::new();
        reg.register(Box::new(StubPlugin {
            id: Language::Cpp,
            exts: &[".cpp", ".h"],
        }))
        .unwrap();
        reg
    }

    fn touch(path: &Path) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, b"").unwrap();
    }

    #[test]
    fn discovers_only_registered_extensions() {
        let dir = TempDir::new().unwrap();
        touch(&dir.path().join("a.cpp"));
        touch(&dir.path().join("b.h"));
        touch(&dir.path().join("c.png"));
        touch(&dir.path().join("d.o"));
        touch(&dir.path().join("nested/e.cpp"));
        touch(&dir.path().join("nested/f.txt"));

        let reg = cpp_only_registry();
        let cfg = DiscoveryConfig::default();
        let result = discover(dir.path(), &reg, &cfg);

        let names: Vec<String> = result
            .files
            .iter()
            .map(|f| f.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 3, "expected 3 .cpp/.h files, got {names:?}");
        assert!(names.iter().any(|n| n == "a.cpp"));
        assert!(names.iter().any(|n| n == "b.h"));
        assert!(names.iter().any(|n| n == "e.cpp"));
        for f in &result.files {
            assert_eq!(f.language, Language::Cpp);
        }
    }

    #[test]
    fn respects_gitignore_when_enabled() {
        let dir = TempDir::new().unwrap();
        touch(&dir.path().join("keep.cpp"));
        touch(&dir.path().join("target/foo.cpp"));
        fs::write(dir.path().join(".gitignore"), "target/\n").unwrap();

        let reg = cpp_only_registry();
        let cfg = DiscoveryConfig::default();
        let result = discover(dir.path(), &reg, &cfg);

        let paths: Vec<String> = result
            .files
            .iter()
            .map(|f| f.path.to_string_lossy().into_owned())
            .collect();
        assert!(
            paths.iter().any(|p| p.ends_with("keep.cpp")),
            "expected keep.cpp in {paths:?}"
        );
        assert!(
            !paths.iter().any(|p| p.contains("target/foo.cpp")),
            "target/foo.cpp must be excluded by .gitignore: {paths:?}"
        );
    }

    #[test]
    fn ignores_gitignore_when_disabled() {
        let dir = TempDir::new().unwrap();
        touch(&dir.path().join("keep.cpp"));
        touch(&dir.path().join("target/foo.cpp"));
        fs::write(dir.path().join(".gitignore"), "target/\n").unwrap();

        let reg = cpp_only_registry();
        let cfg = DiscoveryConfig {
            respect_gitignore: false,
            ..Default::default()
        };
        let result = discover(dir.path(), &reg, &cfg);

        let paths: Vec<String> = result
            .files
            .iter()
            .map(|f| f.path.to_string_lossy().into_owned())
            .collect();
        assert!(paths.iter().any(|p| p.ends_with("keep.cpp")));
        assert!(
            paths.iter().any(|p| p.contains("target/foo.cpp")),
            "target/foo.cpp must be included when respect_gitignore=false: {paths:?}"
        );
    }

    #[test]
    fn extra_ignore_patterns_exclude_matching_files() {
        // Custom ignore globs feed through OverrideBuilder. Users write a
        // positive pattern (`build/**`) and the walker treats it as an
        // ignore rule.
        let dir = TempDir::new().unwrap();
        touch(&dir.path().join("keep.cpp"));
        touch(&dir.path().join("build/skip.cpp"));

        let reg = cpp_only_registry();
        let cfg = DiscoveryConfig {
            extra_ignore: vec!["build/**".to_string()],
            ..Default::default()
        };
        let result = discover(dir.path(), &reg, &cfg);

        let paths: Vec<String> = result
            .files
            .iter()
            .map(|f| f.path.to_string_lossy().into_owned())
            .collect();
        assert!(paths.iter().any(|p| p.ends_with("keep.cpp")));
        assert!(
            !paths.iter().any(|p| p.contains("build/skip.cpp")),
            "build/skip.cpp must be excluded by extra_ignore: {paths:?}"
        );
    }

    #[test]
    fn invalid_extra_ignore_pattern_warns_does_not_abort() {
        let dir = TempDir::new().unwrap();
        touch(&dir.path().join("keep.cpp"));

        let reg = cpp_only_registry();
        let cfg = DiscoveryConfig {
            // `[` with no closing bracket is a glob parse error.
            extra_ignore: vec!["[".to_string()],
            ..Default::default()
        };
        let result = discover(dir.path(), &reg, &cfg);

        // The walk still produced files.
        assert!(
            result.files.iter().any(|f| f.path.ends_with("keep.cpp")),
            "walk must continue past a bad glob pattern: {:?}",
            result.files
        );
        // And the bad pattern surfaced as a warning.
        assert!(
            result
                .warnings
                .iter()
                .any(|w| w.contains("invalid extra_ignore pattern")),
            "expected invalid-pattern warning, got: {:?}",
            result.warnings
        );
    }

    #[test]
    fn follow_symlinks_default_false() {
        // A symlink whose target is the parent directory would loop forever
        // if followed. Default cfg keeps follow_symlinks=false, so the
        // walker must not infinite-loop or duplicate files.
        let dir = TempDir::new().unwrap();
        touch(&dir.path().join("a.cpp"));
        // Create a self-loop symlink: dir/loop -> dir
        #[cfg(unix)]
        {
            let link = dir.path().join("loop");
            std::os::unix::fs::symlink(dir.path(), &link).unwrap();
        }

        let reg = cpp_only_registry();
        let cfg = DiscoveryConfig::default();
        let result = discover(dir.path(), &reg, &cfg);

        // Exactly one a.cpp; no infinite recursion through the symlink.
        let count = result
            .files
            .iter()
            .filter(|f| f.path.file_name().unwrap() == "a.cpp")
            .count();
        assert_eq!(
            count, 1,
            "expected 1 a.cpp, got {count} (files: {:?})",
            result.files
        );
    }

    #[test]
    fn discovery_runs_with_zero_threads_meaning_auto() {
        // `WalkBuilder::threads(0)` means "auto" per the ignore crate.
        // A defensive caller passing a raw 0 (instead of a resolved value)
        // must still get a working walk.
        let dir = TempDir::new().unwrap();
        touch(&dir.path().join("a.cpp"));
        touch(&dir.path().join("b.h"));

        let reg = cpp_only_registry();
        let cfg = DiscoveryConfig {
            max_threads: 0,
            ..Default::default()
        };
        let result = discover(dir.path(), &reg, &cfg);
        assert_eq!(
            result.files.len(),
            2,
            "expected both files: {:?}",
            result.files
        );
    }

    #[cfg(unix)]
    #[test]
    fn walk_warnings_surface() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        touch(&dir.path().join("readable.cpp"));
        let locked = dir.path().join("locked");
        fs::create_dir(&locked).unwrap();
        touch(&locked.join("inside.cpp"));
        // chmod 000: no read, no traverse.
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();

        // If we can still read the locked dir, we're root (or some other
        // privilege escalation is in play) and chmod 000 won't actually
        // produce a walk error. Skip the assertion in that case rather
        // than fail spuriously.
        let running_as_privileged = fs::read_dir(&locked).is_ok();

        let reg = cpp_only_registry();
        let cfg = DiscoveryConfig::default();
        let result = discover(dir.path(), &reg, &cfg);

        // Restore permissions before any assertion can fail and leave the
        // tempdir un-cleanable.
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();

        if running_as_privileged {
            eprintln!("walk_warnings_surface: skipped (running as privileged user)");
            return;
        }

        assert!(
            !result.warnings.is_empty(),
            "expected at least one walk warning from the unreadable directory"
        );
    }

    #[test]
    fn discovery_includes_assertions_mixed_tree() {
        // Synthetic mixed-language tree: 1000 files spread across .cpp,
        // .py, .png, and .o; nested in dirs; with a .gitignore excluding
        // `ignored/`. Only .cpp/.h count (registry has Cpp only).
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join(".gitignore"), "ignored/\n").unwrap();

        let mut expected_cpp = 0;
        for i in 0..250 {
            let bucket = i % 5;
            let sub = format!("d{bucket}");
            touch(&dir.path().join(&sub).join(format!("a{i}.cpp")));
            expected_cpp += 1;
            touch(&dir.path().join(&sub).join(format!("b{i}.py")));
            touch(&dir.path().join(&sub).join(format!("c{i}.png")));
            touch(&dir.path().join(&sub).join(format!("d{i}.o")));
        }
        // Throw a few headers in too.
        for i in 0..10 {
            touch(&dir.path().join("inc").join(format!("h{i}.h")));
            expected_cpp += 1;
        }
        // Files in an ignored dir must not be counted.
        for i in 0..5 {
            touch(&dir.path().join("ignored").join(format!("x{i}.cpp")));
        }

        let reg = cpp_only_registry();
        let cfg = DiscoveryConfig::default();
        let result = discover(dir.path(), &reg, &cfg);

        assert_eq!(
            result.files.len(),
            expected_cpp,
            "expected {expected_cpp} .cpp/.h files, got {} ({:?})",
            result.files.len(),
            result
                .files
                .iter()
                .filter(|f| f.path.to_string_lossy().contains("ignored"))
                .collect::<Vec<_>>()
        );

        // No file from the ignored dir leaked through.
        assert!(
            !result
                .files
                .iter()
                .any(|f| f.path.to_string_lossy().contains("/ignored/")),
            "ignored/ files must be excluded"
        );

        // Sort invariant: files arrive in lexicographic order so the
        // parse-test diff against the Go binary is deterministic.
        for w in result.files.windows(2) {
            assert!(w[0].path <= w[1].path, "files must be sorted by path");
        }
    }

    #[test]
    #[ignore = "timing-based; manual gate. Run with --ignored to confirm parallel walker is at least as fast as a sync walker on a >1000-file tree."]
    fn parallel_walker_faster_than_sync_walker_for_large_tree() {
        use std::time::Instant;
        use walkdir::WalkDir;

        let dir = TempDir::new().unwrap();
        for i in 0..1500 {
            let bucket = i % 20;
            touch(&dir.path().join(format!("d{bucket}/f{i}.cpp")));
        }

        let reg = cpp_only_registry();
        let cfg = DiscoveryConfig::default();

        let t0 = Instant::now();
        let parallel = discover(dir.path(), &reg, &cfg);
        let parallel_dt = t0.elapsed();

        let t1 = Instant::now();
        let mut sync_count = 0usize;
        for e in WalkDir::new(dir.path()) {
            let e = e.unwrap();
            if e.file_type().is_file() && reg.language_for_path(e.path()).is_some() {
                sync_count += 1;
            }
        }
        let sync_dt = t1.elapsed();

        assert_eq!(parallel.files.len(), sync_count);
        eprintln!(
            "parallel: {parallel_dt:?}; sync: {sync_dt:?}; ratio: {:.2}",
            parallel_dt.as_secs_f64() / sync_dt.as_secs_f64()
        );
    }
}
