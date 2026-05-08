//! Language plugin trait and registry.
//!
//! [`LanguagePlugin`] is the per-language interface — the Rust analogue of
//! the Go `parser.Parser` interface in `internal/parser/parser.go`. The
//! [`LanguageRegistry`] maps file extensions to plugins; it mirrors the Go
//! `parser.Registry` in `internal/parser/registry.go`.
//!
//! Phase 3.3 wires up the real default impls of
//! [`LanguagePlugin::resolve_call`] and [`LanguagePlugin::resolve_include`].
//! The scope-aware resolver and the basename resolver port the Go
//! reference at `internal/tools/analyze.go` (`resolveCall` and
//! `resolveInclude`) byte-for-byte, including a known dead-code path in
//! `resolveCall` that the Go implementation kept. The supporting types
//! [`CallContext`], [`SymbolIndex`], [`FileIndex`] now carry real fields
//! populated by the Phase 3.3 indexer.

pub mod helpers;

use code_graph_core::{ExtensionsConfig, FileGraph, Language, RootConfig, SymbolId};
use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Errors a [`LanguagePlugin`] may return from [`LanguagePlugin::parse_file`].
///
/// `Io` is for file-level I/O (rare here — content is normally pre-read by
/// the discovery layer and passed in as `&[u8]` — but kept on the surface
/// for plugins that touch the filesystem during parse, e.g. include lookups).
/// `Parse` covers grammar errors that the plugin chooses to surface rather
/// than skip silently. `Query` covers tree-sitter query compilation/execution
/// failures.
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("parse error: {0}")]
    Parse(String),
    #[error("query error: {0}")]
    Query(String),
}

/// Errors returned by [`LanguageRegistry::register`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RegistryError {
    /// Two plugins claimed the same extension. The string is the lowercased
    /// extension that collided (e.g. `".cpp"`).
    #[error("extension {0:?} is already registered")]
    DuplicateExtension(String),
    /// A plugin was registered for a [`Language`] that already has a plugin.
    /// The first plugin's extensions would otherwise resolve to a different
    /// (replacement) plugin — see the validation pass in
    /// [`LanguageRegistry::register`].
    #[error("language {0:?} is already registered")]
    DuplicateLanguage(Language),
    /// A plugin returned an extension that did not start with `.`. The trait
    /// contract requires the leading dot — without it, `language_for_path`
    /// would never match the file because lookup uses `format!(".{ext}")`.
    #[error("invalid extension {extension:?}: {reason}")]
    InvalidExtension {
        extension: String,
        reason: &'static str,
    },
}

// Edge-resolution support types -------------------------------------------
// Populated by the Phase 3.3 indexer (`code-graph-tools::indexer`) and
// consumed by the default impls of [`LanguagePlugin::resolve_call`] /
// [`LanguagePlugin::resolve_include`]. Per-language plugins may override the
// trait methods to add language-specific scoping.

/// Context passed to [`LanguagePlugin::resolve_call`]. The indexer fills this
/// in once per call edge from the edge's `from` symbol ID and the file the
/// edge was emitted from.
///
/// `caller_id` is the full symbol ID of the caller in `file:Name` or
/// `file:Parent::Name` form. `caller_file` is the absolute path of the file
/// the call appears in (the resolver awards a same-file bonus to candidates
/// whose own file matches). `language` scopes the [`SymbolIndex`] lookup to
/// candidates from the same language so a Python `init` never collides with
/// a C++ `init`.
#[derive(Debug, Clone)]
pub struct CallContext<'a> {
    /// Full symbol ID of the caller. Format is `file:Name` or
    /// `file:Parent::Name`. Used to derive the caller's parent class for
    /// the same-parent bonus.
    pub caller_id: &'a str,
    /// Absolute path of the file the call appears in. Used for the
    /// same-file bonus.
    pub caller_file: &'a Path,
    /// Language to scope candidate lookup to. The indexer keys
    /// [`SymbolIndex`] by `(Language, name)` so cross-language collisions
    /// are impossible.
    pub language: Language,
}

/// One candidate for [`SymbolIndex`] lookup — a symbol that bears the name
/// being resolved.
#[derive(Clone, Debug)]
pub struct SymbolEntry {
    /// Stable graph ID — `file:Name` or `file:Parent::Name`.
    pub id: SymbolId,
    /// Absolute path of the file declaring this symbol.
    pub file: PathBuf,
    /// Parent class/struct, if any. Empty string for free functions.
    pub parent: String,
    /// Namespace, if any. Empty string for global-scope symbols.
    pub namespace: String,
}

/// Symbol-name index used by [`LanguagePlugin::resolve_call`] to look up
/// candidate callees by `(Language, name)` and filter by scope.
///
/// Keying by `(Language, String)` instead of `String` alone (the Go shape)
/// makes cross-language collisions impossible during call resolution — a
/// Python `init` and a C++ `init` live in separate buckets and never
/// confuse each other's resolvers.
#[derive(Debug, Default, Clone)]
pub struct SymbolIndex {
    /// Inverted index keyed by `(Language, name)`. The same symbol may be
    /// indexed under several names (bare `Name`, qualified `Parent::Name`,
    /// `Namespace::Name`, etc.) — see `code-graph-tools::indexer::build_symbol_index`.
    pub by_name: HashMap<(Language, String), Vec<SymbolEntry>>,
}

impl SymbolIndex {
    /// Construct an empty index.
    pub fn new() -> Self {
        Self::default()
    }
}

/// File-path index used by [`LanguagePlugin::resolve_include`] to map a raw
/// `#include`/`import` string to a concrete absolute file path. Keyed by
/// basename so an `#include "foo.h"` finds any file named `foo.h` regardless
/// of the directory it lives in.
#[derive(Debug, Default, Clone)]
pub struct FileIndex {
    /// `file_name() → absolute paths` of every discovered file with that
    /// basename. Multiple paths sharing a basename are kept; the resolver
    /// disambiguates by suffix match.
    pub by_basename: HashMap<String, Vec<PathBuf>>,
}

impl FileIndex {
    /// Construct an empty index.
    pub fn new() -> Self {
        Self::default()
    }
}

/// Default scope-aware call resolver. Ports the Go reference at
/// `internal/tools/analyze.go::resolveCall` with one structural fix and one
/// preserved dead branch — see notes below.
///
/// Priority: same file > same parent class > same namespace > any.
///
/// # NOTE: matches the Go reference's resolveCall, including the unreachable same-namespace bonus
///
/// The Go implementation initializes `callerNS` to `""` and never updates
/// it; the same-namespace branch (`score += 2`) is therefore unreachable.
/// The trailing `_ = callerNS` line in Go suppresses the unused-variable
/// warning. We replicate the dead branch verbatim here so this resolver
/// produces byte-identical edge resolution against Go-binary baselines for
/// the cases where it matters — the parity gates in Phase 3.2 and Phase
/// 3.7 depend on this match.
///
/// The same-parent extraction in [`caller_id_parent`] uses a
/// singleton-colon rule rather than Go's `strings.LastIndex(":")`. The Go
/// helper has a subtle bug: `LastIndex(":")` on `file:Foo::bar` returns
/// the index of the *second* `:` in `::`, so its same-parent branch is
/// also dead in practice for canonical method IDs. The Rust helper picks
/// the path/symbol boundary correctly (and still handles Windows drive
/// letters), making the same-parent bonus actually functional.
pub(crate) fn default_scope_aware_resolve(
    language: Language,
    callee: &str,
    ctx: &CallContext,
    index: &SymbolIndex,
) -> Option<SymbolId> {
    let candidates = index.by_name.get(&(language, callee.to_string()))?;
    if candidates.is_empty() {
        return None;
    }
    if candidates.len() == 1 {
        return Some(candidates[0].id.clone());
    }

    let caller_parent = caller_id_parent(ctx.caller_id);
    // NOTE: matches the Go reference's resolveCall, including the
    // unreachable same-namespace bonus. Initialized to "" and never
    // updated — preserved for parity with the Go binary.
    let caller_ns: String = String::new();

    let mut best: Option<&SymbolEntry> = None;
    let mut best_score: i32 = -1;
    for c in candidates {
        let mut score = 0i32;
        if c.file == ctx.caller_file {
            score += 4;
        }
        if !caller_parent.is_empty() && c.parent == caller_parent {
            score += 3;
        }
        if !caller_ns.is_empty() && c.namespace == caller_ns {
            score += 2;
        }
        if score > best_score {
            best_score = score;
            best = Some(c);
        }
    }
    let _ = caller_ns; // mirror Go's `_ = callerNS` line
    best.map(|e| e.id.clone())
}

/// Extract the parent class name from a caller's symbol ID.
///
/// Symbol IDs have the shape `file:Name` (free function) or
/// `file:Parent::Name` (method). For methods this returns `"Parent"`; for
/// free functions and unparseable IDs it returns `""`.
///
/// We need to find the colon that separates the path from the symbol name.
/// On Windows the path can itself contain colons (`C:\proj\foo.cpp`), and
/// the symbol name can contain `::` scope separators. The path/symbol
/// boundary is the LAST *singleton* `:` — a colon that is neither
/// immediately preceded nor immediately followed by another `:`. The `::`
/// scope separator and the Windows drive `:` are both correctly skipped
/// by this rule.
fn caller_id_parent(caller_id: &str) -> String {
    let bytes = caller_id.as_bytes();
    let mut sep: Option<usize> = None;
    for (i, &b) in bytes.iter().enumerate() {
        if b != b':' {
            continue;
        }
        let prev_is_colon = i > 0 && bytes[i - 1] == b':';
        let next_is_colon = i + 1 < bytes.len() && bytes[i + 1] == b':';
        if !prev_is_colon && !next_is_colon {
            sep = Some(i);
        }
    }
    let Some(idx) = sep else {
        return String::new();
    };
    let name = &caller_id[idx + 1..];
    if let Some(scope_end) = name.find("::") {
        return name[..scope_end].to_string();
    }
    String::new()
}

/// Default basename-based include resolver. Mirrors the Go reference at
/// `internal/tools/analyze.go::resolveInclude` byte-for-byte.
///
/// 1. Look up candidates by `file_name()` of the raw include path.
/// 2. If exactly one candidate matches, return it.
/// 3. If multiple candidates share the basename, prefer the one whose
///    full path ends with `/raw` or `\raw` (suffix disambiguation for
///    cases like `#include "foo/bar.h"` matching `.../foo/bar.h` over
///    `.../baz/bar.h`).
/// 4. Otherwise return the first candidate (ambiguous).
pub(crate) fn default_basename_resolve(raw: &str, file_index: &FileIndex) -> Option<PathBuf> {
    let base = std::path::Path::new(raw).file_name()?.to_str()?;
    let candidates = file_index.by_basename.get(base)?;
    if candidates.len() == 1 {
        return Some(candidates[0].clone());
    }
    if candidates.len() > 1 {
        for c in candidates {
            let cs = c.to_string_lossy();
            if cs.ends_with(&format!("/{raw}")) || cs.ends_with(&format!("\\{raw}")) {
                return Some(c.clone());
            }
        }
        return Some(candidates[0].clone());
    }
    None
}

/// A per-language source-file parser. Implementations are constructed once
/// (typically in `LanguageRegistry::register`) and shared across threads —
/// hence the `Send + Sync` bound.
///
/// The trait is **object-safe**: `Box<dyn LanguagePlugin>` is the canonical
/// storage form in [`LanguageRegistry`]. The unit tests in this crate prove
/// object safety at compile time.
pub trait LanguagePlugin: Send + Sync {
    /// The language this plugin handles.
    fn id(&self) -> Language;

    /// File extensions this plugin claims. Each extension MUST start with a
    /// `.` (e.g. `".cpp"`). The registry lowercases extensions before
    /// matching, so plugins may return either case here.
    fn extensions(&self) -> &'static [&'static str];

    /// Pre-parse hook for byte-level transformations (macro stripping,
    /// preprocessor shims, etc.). Default impl borrows the input
    /// unchanged — zero-cost for plugins that don't need it. The C++
    /// plugin overrides this to apply `[cpp].macro_strip` substitutions.
    fn preprocess<'a>(&self, content: &'a [u8], _cfg: &RootConfig) -> Cow<'a, [u8]> {
        Cow::Borrowed(content)
    }

    /// Parse a single file. `path` is the absolute file path used for symbol
    /// IDs; `content` is the raw file bytes.
    fn parse_file(&self, path: &Path, content: &[u8]) -> Result<FileGraph, ParseError>;

    /// Language-specific call resolution. The default mirrors the Go scope
    /// heuristic (same file > same parent > same namespace > global) and
    /// scopes the lookup to candidates from the same [`Language`] as the
    /// caller. Languages override to add language-specific scoping (e.g.
    /// Python prefers same-module).
    fn resolve_call(
        &self,
        callee: &str,
        ctx: &CallContext,
        index: &SymbolIndex,
    ) -> Option<SymbolId> {
        default_scope_aware_resolve(self.id(), callee, ctx, index)
    }

    /// Optional language-specific include/import resolution. Default is
    /// basename-based path matching with a suffix-disambiguation pass.
    /// Languages with package systems (Go modules, Python dotted imports)
    /// should override.
    fn resolve_include(&self, raw: &str, file_index: &FileIndex) -> Option<PathBuf> {
        default_basename_resolve(raw, file_index)
    }

    /// Release any resources held by the plugin (e.g. tree-sitter queries).
    /// Default is a no-op; tree-sitter `Query` already drops cleanly.
    fn close(&self) {}
}

/// Maps file extensions to language plugins.
///
/// Two layers of lookup:
/// 1. `by_ext`: extension (lowercased, leading `.`) → [`Language`]
/// 2. `plugins`: [`Language`] → boxed plugin
///
/// The split lets callers ask "what language is this file?" cheaply (without
/// borrowing the plugin) before deciding whether to dispatch to it.
pub struct LanguageRegistry {
    by_ext: HashMap<String, Language>,
    plugins: HashMap<Language, Box<dyn LanguagePlugin>>,
}

impl LanguageRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            by_ext: HashMap::new(),
            plugins: HashMap::new(),
        }
    }

    /// Register a plugin for all of its declared extensions. Extensions are
    /// lowercased before insertion. Returns
    /// [`RegistryError::DuplicateExtension`] if any extension is already
    /// claimed (by this plugin or a previously-registered one) — matching
    /// the Go `Register` behavior. Returns
    /// [`RegistryError::DuplicateLanguage`] if a plugin for the same
    /// [`Language`] is already registered (without this guard, a second
    /// plugin would silently replace the first while the first plugin's
    /// extensions stayed mapped). Returns [`RegistryError::InvalidExtension`]
    /// if any extension does not start with `.` — the lookup path uses
    /// `format!(".{ext}")` and a dotless extension would never match.
    ///
    /// On error the registry is unchanged: the plugin is dropped and no
    /// extensions are registered. (Implementation note: we validate every
    /// extension up front before mutating `by_ext` or `plugins`.)
    pub fn register(&mut self, plugin: Box<dyn LanguagePlugin>) -> Result<(), RegistryError> {
        let lang = plugin.id();
        let exts: Vec<String> = plugin
            .extensions()
            .iter()
            .map(|e| e.to_ascii_lowercase())
            .collect();

        // First pass: validate before mutating anything.
        if self.plugins.contains_key(&lang) {
            return Err(RegistryError::DuplicateLanguage(lang));
        }
        let mut seen = std::collections::HashSet::with_capacity(exts.len());
        for ext in &exts {
            if !ext.starts_with('.') {
                return Err(RegistryError::InvalidExtension {
                    extension: ext.clone(),
                    reason: "extension must start with '.'",
                });
            }
            if self.by_ext.contains_key(ext) || !seen.insert(ext.clone()) {
                return Err(RegistryError::DuplicateExtension(ext.clone()));
            }
        }

        // Second pass: commit.
        for ext in exts {
            self.by_ext.insert(ext, lang);
        }
        self.plugins.insert(lang, plugin);
        Ok(())
    }

    /// Return the plugin that claims this file's extension, or `None` if no
    /// plugin matches (or the path has no extension).
    pub fn for_path(&self, p: &Path) -> Option<&dyn LanguagePlugin> {
        let lang = self.language_for_path(p)?;
        self.plugins.get(&lang).map(|b| &**b)
    }

    /// Return the language tag for a file path based on its extension, or
    /// `None` if no plugin claims this extension. Cheaper than
    /// [`Self::for_path`] when the caller only needs the language.
    pub fn language_for_path(&self, p: &Path) -> Option<Language> {
        let ext = p.extension()?.to_str()?.to_ascii_lowercase();
        // Stored keys include the leading dot to match Go's filepath.Ext.
        let key = format!(".{ext}");
        self.by_ext.get(&key).copied()
    }

    /// Look up a plugin by language tag, bypassing the extension lookup.
    /// Useful when the caller already has a [`Language`] in hand.
    pub fn plugin_for(&self, lang: Language) -> Option<&dyn LanguagePlugin> {
        self.plugins.get(&lang).map(|b| &**b)
    }

    /// Like [`Self::language_for_path`], but consults the per-root
    /// `[extensions]` config first. Order of precedence:
    ///
    /// 1. **`[extensions].disabled`**: matched extensions return `None`
    ///    even when a plugin (default or additive) would have claimed
    ///    them. The whole point of the disabled list is to drop files
    ///    from discovery.
    /// 2. **`[extensions].<lang>` (additive)**: matched extensions resolve
    ///    to the user-specified language even when a plugin's defaults
    ///    would have claimed them. Users override deliberately.
    /// 3. **Plugin defaults**: the registry's static `by_ext` map.
    ///
    /// Cross-additive collisions (same extension in two `[extensions].<lang>`
    /// lists) are caught at config-load time, so this dispatch never has
    /// to disambiguate between two additive claims.
    pub fn language_for_path_with_config(
        &self,
        p: &Path,
        cfg: &ExtensionsConfig,
    ) -> Option<Language> {
        let ext = p.extension()?.to_str()?.to_ascii_lowercase();
        let key = format!(".{ext}");
        if cfg.is_disabled(&key) {
            return None;
        }
        if let Some(lang) = cfg.lookup_additional(&key) {
            return Some(lang);
        }
        self.by_ext.get(&key).copied()
    }

    /// Like [`Self::for_path`], but consults the per-root `[extensions]`
    /// config. See [`Self::language_for_path_with_config`] for precedence
    /// rules.
    pub fn for_path_with_config(
        &self,
        p: &Path,
        cfg: &ExtensionsConfig,
    ) -> Option<&dyn LanguagePlugin> {
        let lang = self.language_for_path_with_config(p, cfg)?;
        self.plugins.get(&lang).map(|b| &**b)
    }
}

impl Default for LanguageRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use code_graph_core::FileGraph;

    /// Compile-time object-safety check. If `LanguagePlugin` ever stops
    /// being object-safe this will fail to compile.
    fn _object_safety_check() {
        fn assert_object_safe<T: ?Sized>() {}
        assert_object_safe::<dyn LanguagePlugin>();
    }

    /// Test plugin claiming a few synthetic extensions. Used to drive
    /// registry tests without a real tree-sitter parser.
    struct FakePlugin {
        id: Language,
        exts: &'static [&'static str],
    }

    impl LanguagePlugin for FakePlugin {
        fn id(&self) -> Language {
            self.id
        }
        fn extensions(&self) -> &'static [&'static str] {
            self.exts
        }
        fn parse_file(&self, path: &Path, _content: &[u8]) -> Result<FileGraph, ParseError> {
            // Phase-1 test plugin: produce an empty FileGraph so we exercise
            // the trait surface without depending on tree-sitter yet.
            Ok(FileGraph {
                path: path.to_string_lossy().into_owned(),
                language: self.id,
                symbols: Vec::new(),
                edges: Vec::new(),
            })
        }
    }

    fn fake(id: Language, exts: &'static [&'static str]) -> Box<dyn LanguagePlugin> {
        Box::new(FakePlugin { id, exts })
    }

    #[test]
    fn register_and_lookup_known_extension() {
        let mut reg = LanguageRegistry::new();
        reg.register(fake(Language::Cpp, &[".fake", ".fk"]))
            .unwrap();

        let p = Path::new("/tmp/foo.fake");
        let plugin = reg.for_path(p).expect("known extension must resolve");
        assert_eq!(plugin.id(), Language::Cpp);

        // Both extensions resolve.
        let p2 = Path::new("/tmp/bar.fk");
        assert_eq!(reg.for_path(p2).map(|p| p.id()), Some(Language::Cpp));
    }

    #[test]
    fn lookup_is_case_insensitive() {
        let mut reg = LanguageRegistry::new();
        reg.register(fake(Language::Cpp, &[".fake"])).unwrap();
        let p = Path::new("/tmp/foo.FAKE");
        assert_eq!(reg.for_path(p).map(|p| p.id()), Some(Language::Cpp));
    }

    #[test]
    fn unknown_extension_returns_none() {
        let mut reg = LanguageRegistry::new();
        reg.register(fake(Language::Cpp, &[".fake"])).unwrap();
        assert!(reg.for_path(Path::new("/tmp/foo.xyz")).is_none());
        assert!(reg.language_for_path(Path::new("/tmp/foo.xyz")).is_none());
    }

    #[test]
    fn no_extension_returns_none() {
        let mut reg = LanguageRegistry::new();
        reg.register(fake(Language::Cpp, &[".fake"])).unwrap();
        assert!(reg.for_path(Path::new("/tmp/Makefile")).is_none());
        assert!(reg.language_for_path(Path::new("/tmp/Makefile")).is_none());
    }

    #[test]
    fn duplicate_extension_across_plugins_errors() {
        let mut reg = LanguageRegistry::new();
        reg.register(fake(Language::Cpp, &[".fake"])).unwrap();
        let err = reg
            .register(fake(Language::Rust, &[".fake"]))
            .expect_err("duplicate extension must error");
        match err {
            RegistryError::DuplicateExtension(ext) => assert_eq!(ext, ".fake"),
            other => panic!("expected DuplicateExtension, got {other:?}"),
        }
    }

    #[test]
    fn duplicate_language_errors_and_leaves_registry_unchanged() {
        // Two different plugins for the same Language must not silently
        // replace each other. The first plugin's extensions stay mapped to
        // it; the second registration must fail.
        let mut reg = LanguageRegistry::new();
        reg.register(fake(Language::Cpp, &[".one", ".two"]))
            .unwrap();

        let err = reg
            .register(fake(Language::Cpp, &[".alpha", ".beta"]))
            .expect_err("duplicate Language must error");
        match err {
            RegistryError::DuplicateLanguage(lang) => assert_eq!(lang, Language::Cpp),
            other => panic!("expected DuplicateLanguage, got {other:?}"),
        }

        // Registry unchanged: first plugin's extensions still resolve.
        assert_eq!(
            reg.for_path(Path::new("/tmp/x.one")).map(|p| p.id()),
            Some(Language::Cpp)
        );
        assert_eq!(
            reg.for_path(Path::new("/tmp/x.two")).map(|p| p.id()),
            Some(Language::Cpp)
        );
        // Second plugin's would-be extensions never registered.
        assert!(reg.for_path(Path::new("/tmp/x.alpha")).is_none());
        assert!(reg.for_path(Path::new("/tmp/x.beta")).is_none());
    }

    #[test]
    fn missing_leading_dot_extension_errors() {
        // The trait contract says extensions MUST start with '.'. A plugin
        // returning `["cpp"]` (no dot) must be rejected at register-time
        // rather than silently registered and never matching anything.
        let mut reg = LanguageRegistry::new();
        let err = reg
            .register(fake(Language::Cpp, &["cpp"]))
            .expect_err("dotless extension must error");
        match err {
            RegistryError::InvalidExtension { extension, .. } => {
                assert_eq!(extension, "cpp");
            }
            other => panic!("expected InvalidExtension, got {other:?}"),
        }
        // Failed registration must leave the registry empty.
        assert!(reg.for_path(Path::new("/tmp/foo.cpp")).is_none());
        assert!(reg.plugin_for(Language::Cpp).is_none());
    }

    #[test]
    fn duplicate_extension_within_one_plugin_errors() {
        // A plugin declaring `.foo` twice — even with different cases — must
        // error rather than silently dedupe.
        let mut reg = LanguageRegistry::new();
        let err = reg
            .register(fake(Language::Cpp, &[".foo", ".FOO"]))
            .expect_err("intra-plugin duplicate must error");
        match err {
            RegistryError::DuplicateExtension(ext) => assert_eq!(ext, ".foo"),
            other => panic!("expected DuplicateExtension, got {other:?}"),
        }
        // Failed registration must leave the registry empty.
        assert!(reg.for_path(Path::new("/tmp/x.foo")).is_none());
        assert!(reg.plugin_for(Language::Cpp).is_none());
    }

    #[test]
    fn language_for_path_returns_language_only() {
        let mut reg = LanguageRegistry::new();
        reg.register(fake(Language::Rust, &[".rs"])).unwrap();
        assert_eq!(
            reg.language_for_path(Path::new("/x/y.rs")),
            Some(Language::Rust)
        );
    }

    #[test]
    fn plugin_for_language_lookup() {
        let mut reg = LanguageRegistry::new();
        reg.register(fake(Language::Go, &[".go"])).unwrap();
        let plugin = reg.plugin_for(Language::Go).expect("registered language");
        assert_eq!(plugin.id(), Language::Go);
        assert!(reg.plugin_for(Language::Python).is_none());
    }

    #[test]
    fn registry_holds_trait_objects() {
        // Stores Box<dyn LanguagePlugin>: re-confirms object safety beyond
        // the compile-time `_object_safety_check`.
        let mut reg = LanguageRegistry::new();
        reg.register(fake(Language::Cpp, &[".one"])).unwrap();
        reg.register(fake(Language::Rust, &[".two"])).unwrap();
        reg.register(fake(Language::Go, &[".three"])).unwrap();
        reg.register(fake(Language::Python, &[".four"])).unwrap();
        for (lang, ext) in [
            (Language::Cpp, ".one"),
            (Language::Rust, ".two"),
            (Language::Go, ".three"),
            (Language::Python, ".four"),
        ] {
            let p = format!("/tmp/file{ext}");
            assert_eq!(reg.for_path(Path::new(&p)).map(|p| p.id()), Some(lang));
        }
    }

    #[test]
    fn default_resolve_call_returns_none_for_unknown_callee() {
        let plugin = FakePlugin {
            id: Language::Cpp,
            exts: &[".fake"],
        };
        let caller_file = PathBuf::from("/tmp/foo.cpp");
        let ctx = CallContext {
            caller_id: "/tmp/foo.cpp:caller",
            caller_file: &caller_file,
            language: Language::Cpp,
        };
        let idx = SymbolIndex::new();
        // Empty index: nothing to resolve to.
        assert!(plugin.resolve_call("any_callee", &ctx, &idx).is_none());
    }

    #[test]
    fn default_resolve_include_returns_none_for_unknown_basename() {
        let plugin = FakePlugin {
            id: Language::Cpp,
            exts: &[".fake"],
        };
        let idx = FileIndex::new();
        // Empty index: no basenames known.
        assert!(plugin.resolve_include("foo.h", &idx).is_none());
    }

    #[test]
    fn close_default_is_noop() {
        let plugin = FakePlugin {
            id: Language::Cpp,
            exts: &[".fake"],
        };
        // Just calling it is the test — must not panic.
        plugin.close();
    }

    #[test]
    fn parse_file_via_trait_object() {
        // Confirms parse_file is callable through `&dyn LanguagePlugin`.
        let mut reg = LanguageRegistry::new();
        reg.register(fake(Language::Cpp, &[".fake"])).unwrap();
        let path = Path::new("/tmp/sample.fake");
        let plugin = reg.for_path(path).unwrap();
        let fg = plugin.parse_file(path, b"").unwrap();
        assert_eq!(fg.language, Language::Cpp);
        assert_eq!(fg.path, "/tmp/sample.fake");
    }

    // -- Edge resolver tests (Phase 3.3) ---------------------------------

    fn entry(id: &str, file: &str, parent: &str, namespace: &str) -> SymbolEntry {
        SymbolEntry {
            id: id.to_string(),
            file: PathBuf::from(file),
            parent: parent.to_string(),
            namespace: namespace.to_string(),
        }
    }

    fn build_index(items: Vec<(Language, &str, SymbolEntry)>) -> SymbolIndex {
        let mut idx = SymbolIndex::new();
        for (lang, name, entry) in items {
            idx.by_name
                .entry((lang, name.to_string()))
                .or_default()
                .push(entry);
        }
        idx
    }

    #[test]
    fn default_scope_aware_resolve_picks_same_file_over_global() {
        // Two `helper` candidates in different files. Caller is in file A;
        // the same-file bonus (+4) must win over the global no-bonus
        // candidate.
        let same_file = entry("/proj/a.cpp:helper", "/proj/a.cpp", "", "");
        let other_file = entry("/proj/b.cpp:helper", "/proj/b.cpp", "", "");
        let idx = build_index(vec![
            (Language::Cpp, "helper", other_file),
            (Language::Cpp, "helper", same_file.clone()),
        ]);

        let caller_file = PathBuf::from("/proj/a.cpp");
        let ctx = CallContext {
            caller_id: "/proj/a.cpp:caller",
            caller_file: &caller_file,
            language: Language::Cpp,
        };
        let resolved = default_scope_aware_resolve(Language::Cpp, "helper", &ctx, &idx);
        assert_eq!(resolved.as_deref(), Some("/proj/a.cpp:helper"));
    }

    #[test]
    fn default_scope_aware_resolve_picks_same_parent_when_no_same_file() {
        // No same-file candidate, but one shares the caller's parent class
        // (`Engine`). Same-parent bonus (+3) must win over the global
        // no-bonus candidate.
        let same_parent = entry("/proj/b.cpp:Engine::tick", "/proj/b.cpp", "Engine", "");
        let global = entry("/proj/c.cpp:tick", "/proj/c.cpp", "", "");
        let idx = build_index(vec![
            (Language::Cpp, "tick", global),
            (Language::Cpp, "tick", same_parent.clone()),
        ]);

        let caller_file = PathBuf::from("/proj/a.cpp");
        let ctx = CallContext {
            // caller is `Engine::update` declared in a.cpp — different file
            // from both candidates, but same parent as the b.cpp candidate.
            caller_id: "/proj/a.cpp:Engine::update",
            caller_file: &caller_file,
            language: Language::Cpp,
        };
        let resolved = default_scope_aware_resolve(Language::Cpp, "tick", &ctx, &idx);
        assert_eq!(resolved.as_deref(), Some("/proj/b.cpp:Engine::tick"));
    }

    #[test]
    fn default_scope_aware_resolve_returns_none_for_unknown_callee() {
        let idx = SymbolIndex::new();
        let caller_file = PathBuf::from("/proj/a.cpp");
        let ctx = CallContext {
            caller_id: "/proj/a.cpp:caller",
            caller_file: &caller_file,
            language: Language::Cpp,
        };
        let resolved = default_scope_aware_resolve(Language::Cpp, "nope", &ctx, &idx);
        assert!(resolved.is_none());
    }

    #[test]
    fn default_scope_aware_resolve_isolates_languages() {
        // Same name `init` registered for both Python and C++. A C++ caller
        // must only see the C++ entry — the (Language, name) keying makes
        // this byte-level impossible to mix.
        let py_init = entry("/proj/m.py:init", "/proj/m.py", "", "");
        let cpp_init = entry("/proj/m.cpp:init", "/proj/m.cpp", "", "");
        let idx = build_index(vec![
            (Language::Python, "init", py_init),
            (Language::Cpp, "init", cpp_init.clone()),
        ]);

        let caller_file = PathBuf::from("/proj/other.cpp");
        let ctx = CallContext {
            caller_id: "/proj/other.cpp:caller",
            caller_file: &caller_file,
            language: Language::Cpp,
        };
        let resolved = default_scope_aware_resolve(Language::Cpp, "init", &ctx, &idx);
        assert_eq!(resolved.as_deref(), Some("/proj/m.cpp:init"));

        // And the Python lookup returns the Python entry.
        let py_caller_file = PathBuf::from("/proj/other.py");
        let py_ctx = CallContext {
            caller_id: "/proj/other.py:caller",
            caller_file: &py_caller_file,
            language: Language::Python,
        };
        let resolved_py = default_scope_aware_resolve(Language::Python, "init", &py_ctx, &idx);
        assert_eq!(resolved_py.as_deref(), Some("/proj/m.py:init"));
    }

    #[test]
    fn default_scope_aware_resolve_single_candidate_returns_directly() {
        // Single candidate path: skip scoring entirely and return it.
        let only = entry("/proj/x.cpp:only_one", "/proj/x.cpp", "", "");
        let idx = build_index(vec![(Language::Cpp, "only_one", only)]);

        let caller_file = PathBuf::from("/proj/elsewhere.cpp");
        let ctx = CallContext {
            caller_id: "/proj/elsewhere.cpp:caller",
            caller_file: &caller_file,
            language: Language::Cpp,
        };
        let resolved = default_scope_aware_resolve(Language::Cpp, "only_one", &ctx, &idx);
        assert_eq!(resolved.as_deref(), Some("/proj/x.cpp:only_one"));
    }

    #[test]
    fn default_basename_resolve_unique_match() {
        let mut idx = FileIndex::new();
        idx.by_basename
            .entry("foo.h".to_string())
            .or_default()
            .push(PathBuf::from("/proj/include/foo.h"));
        let resolved = default_basename_resolve("foo.h", &idx);
        assert_eq!(resolved.as_deref(), Some(Path::new("/proj/include/foo.h")));
    }

    #[test]
    fn default_basename_resolve_suffix_disambiguates() {
        // Two `bar.h` files: one at /proj/foo/bar.h, one at /proj/baz/bar.h.
        // Caller wrote `#include "foo/bar.h"` — must resolve to the path
        // ending with `/foo/bar.h`.
        let mut idx = FileIndex::new();
        idx.by_basename
            .entry("bar.h".to_string())
            .or_default()
            .push(PathBuf::from("/proj/baz/bar.h"));
        idx.by_basename
            .entry("bar.h".to_string())
            .or_default()
            .push(PathBuf::from("/proj/foo/bar.h"));
        let resolved = default_basename_resolve("foo/bar.h", &idx);
        assert_eq!(resolved.as_deref(), Some(Path::new("/proj/foo/bar.h")));
    }

    #[test]
    fn default_basename_resolve_returns_none_for_no_match() {
        let idx = FileIndex::new();
        assert!(default_basename_resolve("missing.h", &idx).is_none());
    }

    #[test]
    fn caller_id_parent_extracts_class_from_method_id() {
        // Method-style ID: parent class is between final ':' and '::'.
        assert_eq!(caller_id_parent("file:Foo::bar"), "Foo");
        // Free-function ID: no '::' after the final ':' → empty parent.
        assert_eq!(caller_id_parent("file:plain"), "");
        // Empty input.
        assert_eq!(caller_id_parent(""), "");
        // Windows-style path with `:` after drive letter — the LAST `:`
        // separates symbol name from path, so the drive's `:` is ignored.
        assert_eq!(caller_id_parent(r"C:\proj\foo.cpp:Foo::bar"), "Foo");
        // Windows-style path with a free function.
        assert_eq!(caller_id_parent(r"C:\proj\foo.cpp:plain"), "");
    }

    // --- _with_config dispatch tests --------------------------------------

    #[test]
    fn with_config_falls_back_to_defaults_when_config_empty() {
        let mut reg = LanguageRegistry::new();
        reg.register(fake(Language::Cpp, &[".cpp"])).unwrap();
        let cfg = ExtensionsConfig::default();
        assert_eq!(
            reg.language_for_path_with_config(Path::new("/x.cpp"), &cfg),
            Some(Language::Cpp)
        );
        assert_eq!(
            reg.language_for_path_with_config(Path::new("/x.unknown"), &cfg),
            None
        );
    }

    #[test]
    fn with_config_additive_extension_resolves_to_user_language() {
        let mut reg = LanguageRegistry::new();
        reg.register(fake(Language::Cpp, &[".cpp"])).unwrap();
        let cfg = ExtensionsConfig {
            cpp: vec![".cu".to_string()],
            ..Default::default()
        };
        assert_eq!(
            reg.language_for_path_with_config(Path::new("/x.cu"), &cfg),
            Some(Language::Cpp)
        );
    }

    #[test]
    fn with_config_disabled_blocks_default_claim() {
        let mut reg = LanguageRegistry::new();
        reg.register(fake(Language::Cpp, &[".cpp", ".h"])).unwrap();
        let cfg = ExtensionsConfig {
            disabled: vec![".h".to_string()],
            ..Default::default()
        };
        assert_eq!(
            reg.language_for_path_with_config(Path::new("/x.cpp"), &cfg),
            Some(Language::Cpp)
        );
        assert_eq!(
            reg.language_for_path_with_config(Path::new("/x.h"), &cfg),
            None
        );
    }

    #[test]
    fn with_config_disabled_blocks_additive_claim_too() {
        // Pins precedence: `disabled` wins over an additive on the same
        // extension. The load-time validator deliberately permits this
        // overlap (no `ConfigError::ExtensionConflict`), so the dispatch
        // path is the sole arbiter.
        let mut reg = LanguageRegistry::new();
        reg.register(fake(Language::Cpp, &[".cpp"])).unwrap();
        let cfg = ExtensionsConfig {
            cpp: vec![".cu".to_string()],
            disabled: vec![".cu".to_string()],
            ..Default::default()
        };
        assert_eq!(
            reg.language_for_path_with_config(Path::new("/x.cu"), &cfg),
            None
        );
    }

    #[test]
    fn with_config_additive_overrides_default_claim() {
        // Pins the redirection contract: a plugin's defaults claim `.h`,
        // but the user's additive in `python` wins. (The user wrote it
        // deliberately. `disabled` is the only way to silence `.h`
        // entirely.)
        let mut reg = LanguageRegistry::new();
        reg.register(fake(Language::Cpp, &[".cpp", ".h"])).unwrap();
        reg.register(fake(Language::Python, &[".py"])).unwrap();
        let cfg = ExtensionsConfig {
            python: vec![".h".to_string()],
            ..Default::default()
        };
        assert_eq!(
            reg.language_for_path_with_config(Path::new("/x.h"), &cfg),
            Some(Language::Python),
            "user additive must win over default claim"
        );
    }

    #[test]
    fn with_config_for_path_returns_correct_plugin_after_redirect() {
        // The plugin returned by `for_path_with_config` must match the
        // language the dispatch resolved to (not the default plugin).
        let mut reg = LanguageRegistry::new();
        reg.register(fake(Language::Cpp, &[".cpp"])).unwrap();
        reg.register(fake(Language::Python, &[".py"])).unwrap();
        let cfg = ExtensionsConfig {
            python: vec![".cu".to_string()],
            ..Default::default()
        };
        let plugin = reg
            .for_path_with_config(Path::new("/x.cu"), &cfg)
            .expect("additive .cu must resolve to a plugin");
        assert_eq!(plugin.id(), Language::Python);
    }

    #[test]
    fn with_config_disabled_blocks_csharp_additive_claim() {
        // Pins the disabled-precedence contract for the C# additive list:
        // even when `[extensions].csharp = [".cs"]` deliberately claims
        // `.cs`, a `[extensions].disabled = [".cs"]` entry suppresses
        // dispatch entirely. No C# plugin is registered yet (Phase 2),
        // so this test only exercises the dispatch — it asserts `None`,
        // which is also what we'd see if no plugin is present and no
        // additive claimed the extension. The discriminator is the
        // additive: without `disabled`, `language_for_path_with_config`
        // would return `Some(Language::CSharp)` even with no plugin
        // registered, because the dispatch is purely config-driven.
        let mut reg = LanguageRegistry::new();
        reg.register(fake(Language::Cpp, &[".cpp"])).unwrap();
        let cfg_additive_only = ExtensionsConfig {
            csharp: vec![".cs".to_string()],
            ..Default::default()
        };
        // Sanity check: with only the additive, dispatch resolves to C#.
        assert_eq!(
            reg.language_for_path_with_config(Path::new("/x.cs"), &cfg_additive_only),
            Some(Language::CSharp),
            "additive csharp must claim .cs when not disabled"
        );
        // Disabled wins: with both, dispatch returns None.
        let cfg = ExtensionsConfig {
            csharp: vec![".cs".to_string()],
            disabled: vec![".cs".to_string()],
            ..Default::default()
        };
        assert_eq!(
            reg.language_for_path_with_config(Path::new("/x.cs"), &cfg),
            None,
            "disabled must win over csharp additive"
        );
    }

    #[test]
    fn with_config_disabled_blocks_java_additive_claim() {
        // Symmetric to with_config_disabled_blocks_csharp_additive_claim:
        // pins the same disabled-precedence contract for the Java additive
        // list. Documents that the dispatch behavior is uniform across
        // both new languages.
        let mut reg = LanguageRegistry::new();
        reg.register(fake(Language::Cpp, &[".cpp"])).unwrap();
        let cfg_additive_only = ExtensionsConfig {
            java: vec![".java".to_string()],
            ..Default::default()
        };
        assert_eq!(
            reg.language_for_path_with_config(Path::new("/x.java"), &cfg_additive_only),
            Some(Language::Java),
            "additive java must claim .java when not disabled"
        );
        let cfg = ExtensionsConfig {
            java: vec![".java".to_string()],
            disabled: vec![".java".to_string()],
            ..Default::default()
        };
        assert_eq!(
            reg.language_for_path_with_config(Path::new("/x.java"), &cfg),
            None,
            "disabled must win over java additive"
        );
    }

    #[test]
    fn with_config_extensionless_file_returns_none() {
        let mut reg = LanguageRegistry::new();
        reg.register(fake(Language::Cpp, &[".cpp"])).unwrap();
        let cfg = ExtensionsConfig {
            cpp: vec![".cu".to_string()],
            ..Default::default()
        };
        assert!(
            reg.language_for_path_with_config(Path::new("/Makefile"), &cfg)
                .is_none()
        );
    }
}
