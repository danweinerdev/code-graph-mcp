//! Rust Crate Module Model (RCMM): pure-logic computation of every
//! indexed `.rs` file's canonical crate-qualified module path.
//!
//! # Purpose
//!
//! The Rust parser's `parse_file` is per-file and crate-blind: it cannot
//! see the file's owning `Cargo.toml`, the crate name, or sibling files. A
//! symbol declared in `src/reactor.rs` of crate `ark-core` therefore comes
//! out of `parse_file` with no crate-qualified namespace. RCMM is the
//! cross-file model that fills in that missing context, consumed at index
//! time by `RustParser::post_index` (Phase 1 Task 1.3) to rewrite each
//! `Symbol.namespace` to the canonical module path.
//!
//! # Pure-logic, filesystem-free
//!
//! This module never calls into `std::fs` itself. Construction takes:
//!
//! 1. The indexed file set (an iterator of absolute `PathBuf`s — the
//!    same set the indexer already knows about).
//! 2. A `read_manifest: impl Fn(&Path) -> Option<String>` callback that
//!    returns the bytes of a `Cargo.toml` for a given path. The
//!    production wiring (Task 1.3) passes `|p| std::fs::read_to_string(p).ok()`;
//!    tests pass an in-memory `HashMap<PathBuf, String>` lookup so they
//!    can exercise every rule without touching disk.
//!
//! This is deliberate. Filesystem-backed unit tests are slow, flaky on
//! Windows path semantics, and obscure the model logic; the callback seam
//! keeps the rule set under test.
//!
//! # Scope of v1
//!
//! - Root modules: `lib.rs` and `main.rs` only. `[[bin]]` target roots
//!   (whose `path = "..."` lives in `Cargo.toml`) are deliberately
//!   **out of scope** for Task 1.2. If a real codebase needs them, the
//!   `read_manifest` callback already returns the full TOML; a follow-up
//!   can extend [`CargoManifest`] to deserialize `[[bin]]` arrays
//!   without changing this module's public surface.
//! - `#[path = "x.rs"]` overrides live in `.rs` source, not in
//!   `Cargo.toml`. Task 1.2 does **not** parse `.rs` for `#[path]`. A
//!   seam is provided via [`CrateModuleModel::with_path_overrides`] so
//!   Task 1.3 (or Phase 2's `mod` resolution work) can plug in overrides
//!   parsed from the AST without restructuring this module.
//! - Inline `mod foo { ... }` nesting is **not** RCMM's concern. RCMM
//!   exposes a clean file-level prefix (e.g. `ark_core::reactor`); the
//!   parser's existing inline-mod walker composes `::tests` etc. onto
//!   that prefix at namespace-rewrite time (Task 1.3).
//!
//! # Errors
//!
//! - No `Cargo.toml` found for a file's ancestor chain →
//!   [`CrateModuleModel::module_path_for`] returns `None`. Task 1.3
//!   translates that to today's empty-prefix / inline-mod-only fallback.
//! - Malformed `Cargo.toml` → that crate is skipped (its files get
//!   `None`); one `eprintln!` per malformed manifest is emitted (this
//!   workspace deliberately has no `tracing` dep — see CLAUDE.md
//!   "Logging").
//! - Workspace `Cargo.toml`s (no `[package]` section) → skipped without
//!   warning (a workspace root that only declares `[workspace]` is
//!   legitimate, not malformed). The per-member `Cargo.toml`s are what
//!   carry crate identity.
//!
//! # Dead-code allowance during Task 1.2
//!
//! This module ships as a pure-logic foundation; nothing in the parent
//! crate consumes it yet. Task 1.3 wires `RustParser::post_index` into
//! `CrateModuleModel::build` + `module_path_for`. Until then, rustc
//! would emit unused-warnings that fail `clippy -D warnings` in CI. The
//! `#[allow(dead_code)]` below is the documented seam — it lifts when
//! Task 1.3 lands. Unit tests *do* exercise every public item, but they
//! live in `#[cfg(test)]` and rustc considers that "unused" for the
//! release build.

#![allow(
    dead_code,
    reason = "Task 1.2 is the pure-logic foundation; Task 1.3 wires it into RustParser::post_index."
)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Minimal subset of `Cargo.toml` we need to read.
///
/// Both fields are optional because workspace roots routinely omit
/// `[package]` and `name`. A manifest with no `[package].name` is not
/// malformed; it just contributes no crate name.
#[derive(Debug, Default, Deserialize)]
struct CargoManifest {
    #[serde(default)]
    package: Option<CargoPackage>,
}

#[derive(Debug, Default, Deserialize)]
struct CargoPackage {
    #[serde(default)]
    name: Option<String>,
}

/// One discovered crate: its `Cargo.toml` location, the `src/` directory,
/// and the normalized crate name (Cargo's `-` → `_` substitution applied
/// at construction time).
#[derive(Debug, Clone)]
struct CrateInfo {
    /// Absolute path of the crate root directory (the directory that
    /// contains `Cargo.toml`).
    root: PathBuf,
    /// Absolute path of the crate's `src/` directory. RCMM assumes the
    /// canonical Cargo layout; non-`src/` layouts would need a
    /// follow-up.
    src: PathBuf,
    /// Crate name from `[package].name`, with `-` → `_` (Cargo's
    /// canonical conversion for the Rust identifier form).
    name: String,
}

/// The pure-logic Rust Crate Module Model.
///
/// Built once per index pass from the indexed file set + a manifest
/// reader callback. Exposes `module_path_for(file) -> Option<&str>` so
/// consumers (Task 1.3's `RustParser::post_index`) can rewrite symbol
/// namespaces without re-walking the filesystem.
pub struct CrateModuleModel {
    /// Map from each indexed `.rs` file to its canonical crate-qualified
    /// module path. Files outside any discovered crate are deliberately
    /// **absent** from the map (rather than mapped to `None`) so the
    /// `module_path_for` query is a single `HashMap::get`.
    paths: HashMap<PathBuf, String>,
}

impl CrateModuleModel {
    /// Build the model from the indexed file set and a manifest reader.
    ///
    /// `files` is the complete set of indexed files (any extension). The
    /// builder filters internally — `Cargo.toml` files seed crate
    /// discovery, `.rs` files get their module path computed.
    ///
    /// `read_manifest(path)` must return the TOML text for the given
    /// path or `None` if the file cannot be read. Production wiring
    /// passes `|p| std::fs::read_to_string(p).ok()`; tests pass a
    /// `HashMap`-backed closure.
    ///
    /// # Multi-crate workspaces
    ///
    /// Each crate is resolved against its own owning `Cargo.toml` — the
    /// nearest ancestor `Cargo.toml` for a given `.rs` file wins. This
    /// handles workspaces with nested members correctly: a file in
    /// `workspace/crates/a/src/foo.rs` resolves against `crates/a/Cargo.toml`,
    /// not the outer workspace `Cargo.toml`.
    pub fn build<I, F>(files: I, read_manifest: F) -> Self
    where
        I: IntoIterator<Item = PathBuf>,
        F: Fn(&Path) -> Option<String>,
    {
        // Pass 1: collect everything we'll need. Splitting Cargo.toml
        // paths from .rs paths in one walk keeps the API a single
        // iterator argument while letting us drive the two passes
        // independently.
        let mut manifest_paths: Vec<PathBuf> = Vec::new();
        let mut rs_files: Vec<PathBuf> = Vec::new();
        for f in files {
            if f.file_name().and_then(|s| s.to_str()) == Some("Cargo.toml") {
                manifest_paths.push(f);
            } else if f.extension().and_then(|s| s.to_str()) == Some("rs") {
                rs_files.push(f);
            }
        }

        // Pass 2: parse every manifest, build the crate table.
        let crates = build_crates(&manifest_paths, &read_manifest);

        // Pass 3: for each .rs file, find its owning crate (longest
        // matching crate root prefix) and derive the module path.
        let mut paths: HashMap<PathBuf, String> = HashMap::new();
        for rs in &rs_files {
            if let Some(module_path) = derive_module_path(rs, &crates) {
                paths.insert(rs.clone(), module_path);
            }
        }

        Self { paths }
    }

    /// Builder seam for `#[path = "x.rs"]` attribute overrides.
    ///
    /// Task 1.2 does **not** parse `.rs` source for `#[path]` attributes
    /// — that lives in the AST, not in `Cargo.toml`. This method is the
    /// hook Task 1.3 (or Phase 2's `mod` resolution work) will use to
    /// supply overrides parsed from the parser's AST walk.
    ///
    /// `overrides` is a map of `.rs` file path → already-computed
    /// canonical module path. Each entry replaces whatever
    /// `module_path_for` would otherwise return for that file. Files
    /// not in the map are unchanged.
    ///
    /// Builder-pattern semantics: returns `self` so callers can chain
    /// `CrateModuleModel::build(...).with_path_overrides(overrides)`.
    pub fn with_path_overrides<I>(mut self, overrides: I) -> Self
    where
        I: IntoIterator<Item = (PathBuf, String)>,
    {
        for (file, path) in overrides {
            self.paths.insert(file, path);
        }
        self
    }

    /// Canonical crate-qualified module path for the given file, or
    /// `None` if the file is outside any discovered crate (no
    /// `Cargo.toml` in its ancestor chain, or owning crate had a
    /// malformed manifest / missing `[package].name`).
    ///
    /// Examples (with crate name `"ark_core"`):
    ///
    /// | File                              | Returned path        |
    /// |-----------------------------------|----------------------|
    /// | `<crate>/src/lib.rs`              | `ark_core`           |
    /// | `<crate>/src/main.rs`             | `ark_core`           |
    /// | `<crate>/src/foo.rs`              | `ark_core::foo`      |
    /// | `<crate>/src/foo/mod.rs`          | `ark_core::foo`      |
    /// | `<crate>/src/foo/bar.rs`          | `ark_core::foo::bar` |
    pub fn module_path_for(&self, file: &Path) -> Option<&str> {
        self.paths.get(file).map(String::as_str)
    }
}

/// Parse every manifest path and return the crate table, ordered so that
/// deeper crate roots come first. The longest-prefix lookup in
/// [`derive_module_path`] iterates this slice and picks the first crate
/// whose `src/` directory is an ancestor of the file.
///
/// Ordering rationale: in a workspace with nested members
/// (`workspace/Cargo.toml` plus `workspace/crates/a/Cargo.toml`), a file
/// `workspace/crates/a/src/foo.rs` must resolve against the inner
/// member, not the workspace. Sorting by descending `src` depth makes
/// the first ancestor-match the right one.
fn build_crates(
    manifest_paths: &[PathBuf],
    read_manifest: &impl Fn(&Path) -> Option<String>,
) -> Vec<CrateInfo> {
    let mut crates: Vec<CrateInfo> = Vec::new();

    for manifest in manifest_paths {
        let Some(content) = read_manifest(manifest) else {
            // Couldn't read it. The reader callback is responsible for
            // logging I/O errors at its own discretion (production
            // wraps `std::fs::read_to_string(...).ok()` which discards
            // the error); RCMM treats unreadable manifests as absent.
            continue;
        };
        let parsed: CargoManifest = match toml::from_str(&content) {
            Ok(p) => p,
            Err(e) => {
                // Malformed `Cargo.toml`: skip this crate, leave its
                // files unresolved. CLAUDE.md: this workspace
                // deliberately has no `tracing` dep — `eprintln!` is
                // the documented out-of-handler channel.
                eprintln!(
                    "code-graph-mcp: skipping malformed Cargo.toml at {}: {}",
                    manifest.display(),
                    e
                );
                continue;
            }
        };
        let Some(pkg) = parsed.package else {
            // Workspace-only manifest (no `[package]`) — legitimate,
            // not malformed; member manifests carry the crate
            // identities we need. Silent skip.
            continue;
        };
        let Some(raw_name) = pkg.name else {
            // `[package]` present but `name` missing — degenerate but
            // possible in partial / template manifests. Treat like a
            // workspace root: skip without warning.
            continue;
        };
        let Some(root) = manifest.parent() else {
            // Manifest path has no parent (would be `/Cargo.toml` or
            // bare `Cargo.toml`). Skip — there's no meaningful crate
            // root to anchor to.
            continue;
        };
        // Cargo's canonical conversion: crate names use `-` in
        // `Cargo.toml` but `_` in Rust identifiers and module paths.
        let name = raw_name.replace('-', "_");
        let src = root.join("src");
        crates.push(CrateInfo {
            root: root.to_path_buf(),
            src,
            name,
        });
    }

    // Sort by descending depth of `src` so the longest matching ancestor
    // is found first by `derive_module_path`. Tiebreak by lexicographic
    // path so the order is deterministic across runs.
    crates.sort_by(|a, b| {
        let depth_a = a.src.components().count();
        let depth_b = b.src.components().count();
        depth_b.cmp(&depth_a).then_with(|| a.src.cmp(&b.src))
    });

    // Suppress unused warning on `root` — kept on the struct for
    // future extension (bin target resolution, `[workspace.dependencies]`
    // walks). Using `_ = …` here would discard the field; instead we
    // touch it in a debug-only assertion so the compiler sees the read.
    #[cfg(debug_assertions)]
    for c in &crates {
        debug_assert!(c.src.starts_with(&c.root));
    }

    crates
}

/// For one `.rs` file, find its owning crate and derive the canonical
/// module path. Returns `None` if no crate's `src/` is an ancestor of
/// the file.
fn derive_module_path(file: &Path, crates: &[CrateInfo]) -> Option<String> {
    // Crates are pre-sorted by descending `src` depth; the first
    // ancestor match is the deepest-nesting crate, which is the right
    // owner for files inside workspaces with nested members.
    let owner = crates.iter().find(|c| file.starts_with(&c.src))?;

    // Path of the file relative to the crate's `src/` directory. This
    // is what the module-path rules operate on. `strip_prefix` cannot
    // fail because `starts_with` succeeded above, but we handle the
    // `Err` defensively rather than `expect`-ing — defensive against
    // future refactors that change one side without the other.
    let rel = file.strip_prefix(&owner.src).ok()?;

    // Decompose the relative path into its OS-agnostic components. Each
    // component is a directory or the final filename. We work in
    // `&str`s to keep the joining trivial; any non-UTF-8 component
    // makes the file unresolvable (well outside normal Rust source
    // layouts and not worth a partial-path heuristic).
    let mut comps: Vec<&str> = Vec::new();
    for c in rel.components() {
        let part = c.as_os_str().to_str()?;
        comps.push(part);
    }
    if comps.is_empty() {
        // `src` itself (no file part). Shouldn't happen for an
        // `.rs` file in the indexed set, but treat as unresolved.
        return None;
    }

    // Last component is the filename; strip the `.rs` extension. We've
    // already filtered to .rs files in `build`, but the strip lets us
    // distinguish `lib.rs`/`main.rs`/`mod.rs` from named modules.
    let last = comps.pop()?;
    let stem = last.strip_suffix(".rs")?;

    // Root-module rules. `lib.rs` / `main.rs` at `src/` (no
    // intermediate dirs) → bare crate name. Inside a subdirectory
    // (e.g. `src/foo/lib.rs`) they are NOT root modules — they're
    // ordinary `crate::foo::lib` / `crate::foo::main` files. This
    // matches Cargo's behavior: only `<src>/lib.rs` and `<src>/main.rs`
    // are crate roots in the v1-supported layout.
    if comps.is_empty() && (stem == "lib" || stem == "main") {
        return Some(owner.name.clone());
    }

    // `mod.rs` collapses to its parent directory's name. `<src>/mod.rs`
    // (no intermediate dirs) is invalid Rust — there's no parent to
    // borrow a name from — so we'd have returned `None` via the empty-
    // `comps` guard above. Inside `<src>/foo/mod.rs`, the path is
    // `crate::foo`, NOT `crate::foo::mod`.
    if stem == "mod" {
        let segments: Vec<&str> = std::iter::once(owner.name.as_str())
            .chain(comps.iter().copied())
            .collect();
        return Some(segments.join("::"));
    }

    // Ordinary file: append every directory component plus the stem.
    let segments: Vec<&str> = std::iter::once(owner.name.as_str())
        .chain(comps.iter().copied())
        .chain(std::iter::once(stem))
        .collect();
    Some(segments.join("::"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a model from an in-memory `(file_set, manifest_map)`
    /// fixture. Keeps every test filesystem-free — the whole point of
    /// the reader-callback API is that production passes
    /// `std::fs::read_to_string(...).ok()` while tests pass an inline
    /// HashMap lookup.
    fn build_model(files: Vec<PathBuf>, manifests: Vec<(PathBuf, &str)>) -> CrateModuleModel {
        let manifest_map: HashMap<PathBuf, String> = manifests
            .into_iter()
            .map(|(p, s)| (p, s.to_owned()))
            .collect();
        CrateModuleModel::build(files, |p| manifest_map.get(p).cloned())
    }

    // Minimal valid Cargo.toml fixture text. Real fixtures only need
    // `[package].name`; everything else is optional in v1's logic.
    fn minimal_manifest(name: &str) -> String {
        format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n")
    }

    #[test]
    fn lib_rs_at_src_root_returns_bare_crate_name() {
        let manifest = PathBuf::from("/crate/Cargo.toml");
        let lib = PathBuf::from("/crate/src/lib.rs");
        let model = build_model(
            vec![manifest.clone(), lib.clone()],
            vec![(manifest, &minimal_manifest("ark_core"))],
        );
        assert_eq!(model.module_path_for(&lib), Some("ark_core"));
    }

    #[test]
    fn main_rs_at_src_root_returns_bare_crate_name() {
        let manifest = PathBuf::from("/crate/Cargo.toml");
        let main_rs = PathBuf::from("/crate/src/main.rs");
        let model = build_model(
            vec![manifest.clone(), main_rs.clone()],
            vec![(manifest, &minimal_manifest("ark_core"))],
        );
        assert_eq!(model.module_path_for(&main_rs), Some("ark_core"));
    }

    #[test]
    fn named_file_in_src_returns_crate_double_colon_stem() {
        let manifest = PathBuf::from("/crate/Cargo.toml");
        let foo = PathBuf::from("/crate/src/foo.rs");
        let model = build_model(
            vec![manifest.clone(), foo.clone()],
            vec![(manifest, &minimal_manifest("ark_core"))],
        );
        assert_eq!(model.module_path_for(&foo), Some("ark_core::foo"));
    }

    #[test]
    fn mod_rs_collapses_to_parent_directory_name() {
        let manifest = PathBuf::from("/crate/Cargo.toml");
        let mod_rs = PathBuf::from("/crate/src/foo/mod.rs");
        let model = build_model(
            vec![manifest.clone(), mod_rs.clone()],
            vec![(manifest, &minimal_manifest("ark_core"))],
        );
        assert_eq!(model.module_path_for(&mod_rs), Some("ark_core::foo"));
    }

    #[test]
    fn nested_file_returns_full_dotted_path() {
        let manifest = PathBuf::from("/crate/Cargo.toml");
        let nested = PathBuf::from("/crate/src/a/b.rs");
        let model = build_model(
            vec![manifest.clone(), nested.clone()],
            vec![(manifest, &minimal_manifest("ark_core"))],
        );
        assert_eq!(model.module_path_for(&nested), Some("ark_core::a::b"));
    }

    #[test]
    fn dash_in_crate_name_normalized_to_underscore() {
        let manifest = PathBuf::from("/crate/Cargo.toml");
        let foo = PathBuf::from("/crate/src/foo.rs");
        // `[package].name = "my-cool-crate"` → module path
        // `my_cool_crate::foo`. Cargo's canonical conversion.
        let model = build_model(
            vec![manifest.clone(), foo.clone()],
            vec![(manifest, &minimal_manifest("my-cool-crate"))],
        );
        assert_eq!(model.module_path_for(&foo), Some("my_cool_crate::foo"));
    }

    #[test]
    fn deeply_nested_file_returns_full_chain() {
        let manifest = PathBuf::from("/crate/Cargo.toml");
        let deep = PathBuf::from("/crate/src/a/b/c/d.rs");
        let model = build_model(
            vec![manifest.clone(), deep.clone()],
            vec![(manifest, &minimal_manifest("k"))],
        );
        assert_eq!(model.module_path_for(&deep), Some("k::a::b::c::d"));
    }

    #[test]
    fn mod_rs_deeply_nested_collapses_to_parent_chain() {
        // `<crate>/src/a/b/mod.rs` → `k::a::b` (NOT `k::a::b::mod`).
        let manifest = PathBuf::from("/crate/Cargo.toml");
        let mod_rs = PathBuf::from("/crate/src/a/b/mod.rs");
        let model = build_model(
            vec![manifest.clone(), mod_rs.clone()],
            vec![(manifest, &minimal_manifest("k"))],
        );
        assert_eq!(model.module_path_for(&mod_rs), Some("k::a::b"));
    }

    #[test]
    fn file_outside_any_crate_returns_none() {
        // Indexed file with no `Cargo.toml` anywhere up its ancestor
        // chain. Task 1.3 will translate this `None` to today's empty-
        // prefix / inline-mod-only fallback.
        let lone = PathBuf::from("/standalone/foo.rs");
        let model = build_model(vec![lone.clone()], vec![]);
        assert_eq!(model.module_path_for(&lone), None);
    }

    #[test]
    fn malformed_manifest_skips_crate_without_panic() {
        // `read_manifest` returns the string (the file was readable),
        // but `toml::from_str` rejects it. The crate is skipped, its
        // files get `None`, no panic. We can't easily assert the
        // `eprintln!` content from a unit test, but we can prove the
        // crate was skipped (no module path resolved).
        let manifest = PathBuf::from("/crate/Cargo.toml");
        let foo = PathBuf::from("/crate/src/foo.rs");
        let model = build_model(
            vec![manifest.clone(), foo.clone()],
            vec![(manifest, "this is = not = valid = toml [[[")],
        );
        assert_eq!(model.module_path_for(&foo), None);
    }

    #[test]
    fn workspace_root_without_package_section_is_silently_skipped() {
        // A workspace root `Cargo.toml` with only `[workspace]` is
        // legitimate, not malformed. RCMM should skip it without an
        // `eprintln!` warning, and a file outside any member crate
        // resolves to `None`.
        let ws_manifest = PathBuf::from("/ws/Cargo.toml");
        let stray = PathBuf::from("/ws/src/foo.rs");
        let model = build_model(
            vec![ws_manifest.clone(), stray.clone()],
            vec![(ws_manifest, "[workspace]\nmembers = [\"a\"]\n")],
        );
        assert_eq!(model.module_path_for(&stray), None);
    }

    #[test]
    fn multi_crate_workspace_resolves_each_independently() {
        // Two member crates `a` and `b`; each file resolves against
        // its own owning crate.
        let manifest_a = PathBuf::from("/ws/crates/a/Cargo.toml");
        let manifest_b = PathBuf::from("/ws/crates/b/Cargo.toml");
        let file_a = PathBuf::from("/ws/crates/a/src/foo.rs");
        let file_b = PathBuf::from("/ws/crates/b/src/bar.rs");
        let model = build_model(
            vec![
                manifest_a.clone(),
                manifest_b.clone(),
                file_a.clone(),
                file_b.clone(),
            ],
            vec![
                (manifest_a, &minimal_manifest("a")),
                (manifest_b, &minimal_manifest("b")),
            ],
        );
        assert_eq!(model.module_path_for(&file_a), Some("a::foo"));
        assert_eq!(model.module_path_for(&file_b), Some("b::bar"));
    }

    #[test]
    fn nested_workspace_member_wins_over_outer_workspace_root() {
        // The deepest matching crate root wins for ancestor-prefix
        // matches. Even if a workspace `Cargo.toml` exists at the
        // outer level (here `[workspace]`-only, so it contributes no
        // crate), a member `Cargo.toml` at `crates/a/` must own
        // `crates/a/src/foo.rs`. We also include a hypothetical
        // workspace-root `Cargo.toml` that DID have a `[package]`
        // (the rare "virtual + package" form) to exercise the
        // longest-prefix tiebreak.
        let ws_manifest = PathBuf::from("/ws/Cargo.toml");
        let member_manifest = PathBuf::from("/ws/crates/a/Cargo.toml");
        let file = PathBuf::from("/ws/crates/a/src/foo.rs");
        let model = build_model(
            vec![ws_manifest.clone(), member_manifest.clone(), file.clone()],
            vec![
                (ws_manifest, &minimal_manifest("outer")),
                (member_manifest, &minimal_manifest("a")),
            ],
        );
        // The inner member wins (longest-prefix `src/` match), so the
        // crate name is `a`, not `outer`.
        assert_eq!(model.module_path_for(&file), Some("a::foo"));
    }

    #[test]
    fn manifest_unreadable_skips_crate_without_panic() {
        // The reader callback returns `None` for this manifest path
        // (e.g. permission denied or transient I/O failure). The
        // crate gets no entry, files inside it return `None`. No
        // panic, no warning — the reader is responsible for its own
        // logging.
        let manifest = PathBuf::from("/crate/Cargo.toml");
        let foo = PathBuf::from("/crate/src/foo.rs");
        // Note: `manifests` does NOT contain the manifest path, so
        // the closure returns `None`.
        let model = build_model(vec![manifest, foo.clone()], vec![]);
        assert_eq!(model.module_path_for(&foo), None);
    }

    #[test]
    fn with_path_overrides_replaces_computed_path() {
        // The `#[path]` seam: Task 1.3 (or Phase 2) will call this
        // with overrides parsed from the AST. Here we exercise the
        // hook directly — a file that would naturally resolve to
        // `crate::foo` is overridden to `crate::renamed`.
        let manifest = PathBuf::from("/crate/Cargo.toml");
        let foo = PathBuf::from("/crate/src/foo.rs");
        let model = build_model(
            vec![manifest.clone(), foo.clone()],
            vec![(manifest, &minimal_manifest("k"))],
        )
        .with_path_overrides([(foo.clone(), "k::renamed".to_owned())]);
        assert_eq!(model.module_path_for(&foo), Some("k::renamed"));
    }

    #[test]
    fn with_path_overrides_can_supply_entry_for_unowned_file() {
        // The override map is the only source for files outside any
        // discovered crate IF Task 1.3 chooses to populate it that
        // way (e.g. a `#[path]` pointing at a file the indexer found
        // but no Cargo.toml owns). The seam doesn't require the file
        // to already exist in the model.
        let lone = PathBuf::from("/standalone/foo.rs");
        let model = CrateModuleModel::build(Vec::<PathBuf>::new(), |_| None::<String>)
            .with_path_overrides([(lone.clone(), "explicitly::set".to_owned())]);
        assert_eq!(model.module_path_for(&lone), Some("explicitly::set"));
    }

    #[test]
    fn manifests_in_input_but_no_rs_files_yields_empty_model() {
        // Defensive: a Cargo.toml in the input set but no .rs files
        // inside that crate should not panic and should produce no
        // module paths.
        let manifest = PathBuf::from("/crate/Cargo.toml");
        let model = build_model(
            vec![manifest.clone()],
            vec![(manifest, &minimal_manifest("k"))],
        );
        assert_eq!(model.module_path_for(Path::new("/crate/src/foo.rs")), None);
    }

    #[test]
    fn file_with_non_rs_extension_is_ignored_by_build() {
        // Build filters input to `.rs` and `Cargo.toml`. A `.txt`
        // file slipped into the input set should be silently ignored
        // — no panic, no spurious entry.
        let manifest = PathBuf::from("/crate/Cargo.toml");
        let txt = PathBuf::from("/crate/src/notes.txt");
        let rs = PathBuf::from("/crate/src/foo.rs");
        let model = build_model(
            vec![manifest.clone(), txt.clone(), rs.clone()],
            vec![(manifest, &minimal_manifest("k"))],
        );
        assert_eq!(model.module_path_for(&txt), None);
        assert_eq!(model.module_path_for(&rs), Some("k::foo"));
    }
}
