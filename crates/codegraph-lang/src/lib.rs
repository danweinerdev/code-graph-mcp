//! Language plugin trait and registry.
//!
//! [`LanguagePlugin`] is the per-language interface — the Rust analogue of
//! the Go `parser.Parser` interface in `internal/parser/parser.go`. The
//! [`LanguageRegistry`] maps file extensions to plugins; it mirrors the Go
//! `parser.Registry` in `internal/parser/registry.go`.
//!
//! Phase 1.3 ships the trait surface and the registry. The default impls of
//! [`LanguagePlugin::resolve_call`] and [`LanguagePlugin::resolve_include`]
//! are deliberate Phase-2 stubs returning `None` — the real scope-aware
//! heuristic and basename resolver need the [`SymbolIndex`] / [`FileIndex`]
//! types from Phase 2's graph engine, which haven't shipped yet. The
//! placeholder structs ([`CallContext`], [`SymbolIndex`], [`FileIndex`])
//! are stubs marked clearly as Phase-2 work; they exist now so the trait
//! signature is stable.

use codegraph_core::{FileGraph, Language, SymbolId};
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
pub enum RegistryError {
    /// Two plugins claimed the same extension. The string is the lowercased
    /// extension that collided (e.g. `".cpp"`).
    #[error("extension {0:?} is already registered")]
    DuplicateExtension(String),
}

// Phase-2 placeholder types -----------------------------------------------
// These exist to make the trait signatures compile today. The graph engine
// (Phase 2) will replace each with a real type backed by the live in-memory
// graph. Until then they are pub empty structs with `new()` constructors so
// downstream test code can build trait objects.

/// Context passed to [`LanguagePlugin::resolve_call`]. Populated by the
/// graph engine with the caller's file, parent class, namespace, etc.
///
/// **Phase 2 placeholder.** Today this is empty; Phase 2 fills it in with
/// `caller_file`, `caller_parent`, `caller_namespace` fields and the
/// matching constructor.
// TODO(phase-2): expand to carry caller_file / caller_parent / caller_namespace
// once the Graph engine ships and we know the exact shape `resolve_call`
// needs to consume.
#[derive(Debug, Default, Clone)]
pub struct CallContext {
    _private: (),
}

impl CallContext {
    /// Construct an empty placeholder. Phase 2 replaces this with a builder
    /// fed by the graph engine.
    pub fn new() -> Self {
        Self { _private: () }
    }
}

/// Symbol-name index used by [`LanguagePlugin::resolve_call`] to look up
/// candidate callees by name and filter by scope.
///
/// **Phase 2 placeholder.** Today this is empty; Phase 2 ships the real
/// inverted index built by the graph engine.
// TODO(phase-2): wire to the actual graph engine `SymbolIndex` once Phase 2
// builds the in-memory graph.
#[derive(Debug, Default, Clone)]
pub struct SymbolIndex {
    _private: (),
}

impl SymbolIndex {
    /// Construct an empty placeholder. Phase 2 replaces this with a real
    /// inverted index of symbols by name.
    pub fn new() -> Self {
        Self { _private: () }
    }
}

/// File-path index used by [`LanguagePlugin::resolve_include`] to map a raw
/// `#include`/`import` string to a concrete absolute file path.
///
/// **Phase 2 placeholder.** Today this is empty; Phase 2 ships the real
/// basename → absolute-path mapping.
// TODO(phase-2): wire to the actual graph engine `FileIndex` once Phase 2
// builds the in-memory graph.
#[derive(Debug, Default, Clone)]
pub struct FileIndex {
    _private: (),
}

impl FileIndex {
    /// Construct an empty placeholder.
    pub fn new() -> Self {
        Self { _private: () }
    }
}

/// Phase-2 stub for the default scope-aware call resolver. Mirrors the Go
/// heuristic ordering (same file > same parent > same namespace > global)
/// once the graph engine ships.
///
/// Today this returns `None`. The trait method [`LanguagePlugin::resolve_call`]
/// uses this as its default impl so language plugins inherit the heuristic
/// for free in Phase 2 without changing their signatures now.
// TODO(phase-2): implement the same-file > same-parent > same-namespace >
// global heuristic from internal/graph/resolver.go once SymbolIndex carries
// real data.
pub fn default_scope_aware_resolve(
    _callee: &str,
    _ctx: &CallContext,
    _index: &SymbolIndex,
) -> Option<SymbolId> {
    None
}

/// Phase-2 stub for basename-based include resolution. Returns `None` until
/// Phase 2 wires up [`FileIndex`].
// TODO(phase-2): implement basename matching against FileIndex once Phase 2
// ships the path index. Languages like Go and Python override this entirely;
// C++ relies on the basename heuristic.
pub fn default_basename_resolve(_raw: &str, _file_index: &FileIndex) -> Option<PathBuf> {
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

    /// Parse a single file. `path` is the absolute file path used for symbol
    /// IDs; `content` is the raw file bytes.
    fn parse_file(&self, path: &Path, content: &[u8]) -> Result<FileGraph, ParseError>;

    /// Language-specific call resolution. The default mirrors the Go scope
    /// heuristic (same file > same parent > same namespace > global).
    /// Languages override to add language-specific scoping (e.g. Python
    /// prefers same-module).
    ///
    /// **Phase-2 stub.** The default returns `None` until the graph engine
    /// ships. Plugins should not override this in Phase 1.
    fn resolve_call(
        &self,
        callee: &str,
        ctx: &CallContext,
        index: &SymbolIndex,
    ) -> Option<SymbolId> {
        default_scope_aware_resolve(callee, ctx, index)
    }

    /// Optional language-specific include/import resolution. Default is
    /// basename-based path matching. Languages with package systems (Go
    /// modules, Python dotted imports) should override.
    ///
    /// **Phase-2 stub.** The default returns `None` until the graph engine
    /// ships. Plugins should not override this in Phase 1.
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
    /// the Go `Register` behavior.
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

        // First pass: detect duplicates (against existing entries AND within
        // the plugin's own list — `[".cpp", ".CPP"]` would dedupe on insert
        // and silently drop one, which is worse than an explicit error).
        let mut seen = std::collections::HashSet::with_capacity(exts.len());
        for ext in &exts {
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
}

impl Default for LanguageRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codegraph_core::FileGraph;

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
        }
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
    fn default_resolve_call_returns_none_phase_1_stub() {
        let plugin = FakePlugin {
            id: Language::Cpp,
            exts: &[".fake"],
        };
        let ctx = CallContext::new();
        let idx = SymbolIndex::new();
        // Phase 1: default impl is a stub.
        assert!(plugin.resolve_call("any_callee", &ctx, &idx).is_none());
    }

    #[test]
    fn default_resolve_include_returns_none_phase_1_stub() {
        let plugin = FakePlugin {
            id: Language::Cpp,
            exts: &[".fake"],
        };
        let idx = FileIndex::new();
        // Phase 1: default impl is a stub.
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
}
