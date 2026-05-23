//! Go Module Model (GMM) — the Go-language analog of Rust's RCMM.
//!
//! Discovers `go.mod` files in the indexed file set's ancestor
//! directories, parses each one's `module ...` directive, and
//! builds a [`PathTrie`] keyed by module-root directory. The owning
//! module for a given `.go` file is found via
//! [`PathTrie::longest_prefix`] — `O(file_depth)` per file,
//! independent of how many modules the workspace has.
//!
//! Used by [`crate::GoParser::post_index`] to rewrite `Symbol.namespace`
//! from the bare `package_clause` text (today's behavior) to the
//! canonical Go *import path* of the file's containing directory:
//!
//! | File path                                                  | Result |
//! |------------------------------------------------------------|--------|
//! | `<root>/main.go`               (module `github.com/x/y`)   | `github.com/x/y`           |
//! | `<root>/internal/buf/buf.go`   (module `github.com/x/y`)   | `github.com/x/y/internal/buf` |
//! | `<root>/cmd/cli/main.go`       (module `github.com/x/y`)   | `github.com/x/y/cmd/cli`   |
//! | `<elsewhere>/foo.go`           (no go.mod ancestor)        | unchanged (package name)   |
//!
//! Fall-through is deliberate: files outside any discovered module
//! preserve today's `Symbol.namespace = package_name` behavior so the
//! upgrade is additive, not disruptive.
//!
//! [`PathTrie`]: code_graph_path_trie::PathTrie
//! [`PathTrie::longest_prefix`]: code_graph_path_trie::PathTrie::longest_prefix

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Per-module data: the directory containing `go.mod` and the module
/// name parsed from its `module ...` directive.
#[derive(Debug, Clone)]
struct GoModInfo {
    /// Module-name string from `go.mod`'s `module ...` line. Verbatim;
    /// no path-component normalization (Go module paths are already
    /// canonical slashes).
    name: String,
}

/// Materialized Go-file → import-path lookup. Built once per index
/// pass via [`GoModuleModel::build`]; queried via
/// [`GoModuleModel::namespace_for`].
pub(crate) struct GoModuleModel {
    /// File → fully-qualified import path of its containing directory.
    /// Absent entries fall back to today's bare-package-name namespace
    /// (the `post_index` consumer's responsibility).
    namespaces: HashMap<PathBuf, String>,
}

impl GoModuleModel {
    /// Build the model from the indexed file set + a manifest reader.
    ///
    /// `files` is the complete indexed file set — `go.mod` files seed
    /// module discovery (the indexer normally won't surface them
    /// because no plugin claims `.mod`; the production caller walks
    /// ancestor chains of every `.go` file and adds the `go.mod`
    /// candidates explicitly, mirroring the Rust RCMM pattern).
    ///
    /// `read_manifest` returns the file contents or `None` on read
    /// failure. Production passes `|p| std::fs::read_to_string(p).ok()`;
    /// tests inject a closure backed by an in-memory `HashMap`.
    pub fn build<I, F>(files: I, read_manifest: F) -> Self
    where
        I: IntoIterator<Item = PathBuf>,
        F: Fn(&Path) -> Option<String>,
    {
        let mut go_mod_paths: Vec<PathBuf> = Vec::new();
        let mut go_files: Vec<PathBuf> = Vec::new();
        for f in files {
            if f.file_name().and_then(|s| s.to_str()) == Some("go.mod") {
                go_mod_paths.push(f);
            } else if f.extension().and_then(|s| s.to_str()) == Some("go") {
                go_files.push(f);
            }
        }

        // Build a PathTrie<GoModInfo> keyed by each module-root
        // directory (the dir containing the go.mod). longest_prefix on
        // a file's directory then returns the innermost owning module
        // in O(depth) — same shape as Rust's RCMM. Nested-module
        // layouts (replace directives that put a submodule's go.mod
        // inside the parent's dir tree) are handled by trie structure
        // alone; no separate depth-sort.
        let mut modules: code_graph_path_trie::PathTrie<GoModInfo> =
            code_graph_path_trie::PathTrie::new();
        for manifest in &go_mod_paths {
            let Some(content) = read_manifest(manifest) else {
                // Couldn't read; skip and let downstream files fall
                // back to bare-package-name namespace.
                continue;
            };
            let Some(name) = parse_module_name(&content) else {
                // Malformed go.mod (no `module ...` line). Skip with a
                // warning to stderr — matches the Rust RCMM's
                // "malformed Cargo.toml" handling. Workspace forbids
                // `tracing`; eprintln! is the documented out-of-handler
                // channel.
                eprintln!(
                    "code-graph-mcp: skipping go.mod at {} (no `module` directive found)",
                    manifest.display()
                );
                continue;
            };
            let Some(root) = manifest.parent() else {
                // Bare `go.mod` with no parent dir — degenerate.
                continue;
            };
            modules.insert(root, GoModInfo { name });
        }

        // For each .go file, find its owning module and compose
        // <module_name>/<relative_dir> as the namespace.
        let mut namespaces: HashMap<PathBuf, String> = HashMap::new();
        for go_file in &go_files {
            let Some(dir) = go_file.parent() else {
                continue;
            };
            let Some((root, info)) = modules.longest_prefix(dir) else {
                // No go.mod ancestor → preserve bare-package-name
                // behavior by not inserting an override.
                continue;
            };
            let rel = match dir.strip_prefix(&root) {
                Ok(r) => r,
                Err(_) => continue,
            };
            let namespace = if rel.as_os_str().is_empty() {
                info.name.clone()
            } else {
                // Compose with forward slashes to match Go's import-path
                // convention. `Path::display()` uses platform separators
                // (`\` on Windows); we walk components and join with `/`
                // explicitly so the result is canonical Go.
                let parts: Vec<String> = rel
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect();
                format!("{}/{}", info.name, parts.join("/"))
            };
            namespaces.insert(go_file.clone(), namespace);
        }

        Self { namespaces }
    }

    /// Return the module-qualified namespace (import path) for a file,
    /// or `None` if no `go.mod` was discovered in its ancestor chain.
    pub fn namespace_for(&self, file: &Path) -> Option<&str> {
        self.namespaces.get(file).map(String::as_str)
    }
}

/// Extract the value of the `module ...` directive from a `go.mod`
/// file's contents. Returns `None` if no such directive is present.
///
/// `go.mod` is intentionally a tiny line-based format; full grammar
/// is documented at <https://go.dev/ref/mod#go-mod-file>. We only
/// need the `module` line for namespace derivation, so a one-pass
/// scan (skipping `//` line comments and `/* */` block comments) is
/// adequate.
fn parse_module_name(content: &str) -> Option<String> {
    // Strip `// ...` line comments and `/* ... */` block comments to
    // avoid mistaking a commented `module foo` for the directive. The
    // grammar allows both styles per the upstream spec.
    let cleaned = strip_comments(content);

    for line in cleaned.lines() {
        let line = line.trim();
        // Two valid forms:
        //   module example.com/foo
        //   module "example.com/foo"   (quoted; less common but legal)
        let Some(rest) = line.strip_prefix("module") else {
            continue;
        };
        // Require whitespace separator so we don't match `moduletype` or
        // similar identifier-like substrings.
        if !rest.chars().next().is_some_and(|c| c.is_ascii_whitespace()) {
            continue;
        }
        let val = rest.trim();
        // Strip optional surrounding quotes.
        let bare = val
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .unwrap_or(val);
        if bare.is_empty() {
            return None;
        }
        return Some(bare.to_string());
    }
    None
}

/// Strip `// ...` line comments and `/* ... */` block comments,
/// preserving line counts so any callers that reference the source by
/// line continue to work (the parser doesn't currently — we only walk
/// lines — but cheap insurance against a future cross-reference).
fn strip_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut in_block = false;
    let bytes = src.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if in_block {
            if b == b'*' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
                in_block = false;
                i += 2;
                continue;
            }
            if b == b'\n' {
                out.push('\n');
            }
            i += 1;
            continue;
        }
        if b == b'/' && i + 1 < bytes.len() {
            match bytes[i + 1] {
                b'/' => {
                    // Line comment — skip to next newline (kept so line
                    // counts stay aligned).
                    while i < bytes.len() && bytes[i] != b'\n' {
                        i += 1;
                    }
                    continue;
                }
                b'*' => {
                    in_block = true;
                    i += 2;
                    continue;
                }
                _ => {}
            }
        }
        out.push(b as char);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn closure(map: HashMap<PathBuf, &'static str>) -> impl Fn(&Path) -> Option<String> {
        move |p: &Path| map.get(p).map(|s| s.to_string())
    }

    #[test]
    fn parse_module_name_simple() {
        assert_eq!(
            parse_module_name("module example.com/foo\n").as_deref(),
            Some("example.com/foo")
        );
    }

    #[test]
    fn parse_module_name_with_leading_blank_and_comment() {
        let src = "\n// header comment\nmodule github.com/user/repo\n\ngo 1.21\n";
        assert_eq!(
            parse_module_name(src).as_deref(),
            Some("github.com/user/repo")
        );
    }

    #[test]
    fn parse_module_name_quoted() {
        assert_eq!(
            parse_module_name("module \"example.com/quoted\"\n").as_deref(),
            Some("example.com/quoted")
        );
    }

    #[test]
    fn parse_module_name_ignores_commented_directive() {
        let src = "// module fake.com/decoy\nmodule real.com/value\n";
        assert_eq!(parse_module_name(src).as_deref(), Some("real.com/value"));
    }

    #[test]
    fn parse_module_name_ignores_block_commented_directive() {
        let src = "/* module fake.com/decoy */\nmodule real.com/value\n";
        assert_eq!(parse_module_name(src).as_deref(), Some("real.com/value"));
    }

    #[test]
    fn parse_module_name_returns_none_on_no_directive() {
        assert_eq!(parse_module_name("go 1.21\n"), None);
    }

    #[test]
    fn parse_module_name_returns_none_when_value_empty() {
        assert_eq!(parse_module_name("module \n"), None);
    }

    #[test]
    fn build_resolves_file_at_module_root() {
        let go_mod = PathBuf::from("/repo/go.mod");
        let main_go = PathBuf::from("/repo/main.go");
        let manifests = HashMap::from([(go_mod.clone(), "module github.com/x/y\n")]);
        let m = GoModuleModel::build(vec![go_mod, main_go.clone()], closure(manifests));
        assert_eq!(m.namespace_for(&main_go), Some("github.com/x/y"));
    }

    #[test]
    fn build_resolves_file_in_subdir() {
        let go_mod = PathBuf::from("/repo/go.mod");
        let buf_go = PathBuf::from("/repo/internal/buffer/buffer.go");
        let manifests = HashMap::from([(go_mod.clone(), "module github.com/x/y\n")]);
        let m = GoModuleModel::build(vec![go_mod, buf_go.clone()], closure(manifests));
        assert_eq!(
            m.namespace_for(&buf_go),
            Some("github.com/x/y/internal/buffer")
        );
    }

    #[test]
    fn build_resolves_deeply_nested_file() {
        let go_mod = PathBuf::from("/repo/go.mod");
        let deep = PathBuf::from("/repo/a/b/c/d/x.go");
        let manifests = HashMap::from([(go_mod.clone(), "module mod\n")]);
        let m = GoModuleModel::build(vec![go_mod, deep.clone()], closure(manifests));
        assert_eq!(m.namespace_for(&deep), Some("mod/a/b/c/d"));
    }

    #[test]
    fn build_handles_nested_modules_via_longest_prefix() {
        // Two go.mod files: outer at /repo, inner at /repo/sub.
        // /repo/foo.go → outer module name.
        // /repo/sub/bar.go → inner module name (longest prefix wins).
        let outer_mod = PathBuf::from("/repo/go.mod");
        let inner_mod = PathBuf::from("/repo/sub/go.mod");
        let foo = PathBuf::from("/repo/foo.go");
        let bar = PathBuf::from("/repo/sub/bar.go");
        let manifests = HashMap::from([
            (outer_mod.clone(), "module outer.example/com\n"),
            (inner_mod.clone(), "module inner.example/com\n"),
        ]);
        let m = GoModuleModel::build(
            vec![outer_mod, inner_mod, foo.clone(), bar.clone()],
            closure(manifests),
        );
        assert_eq!(m.namespace_for(&foo), Some("outer.example/com"));
        assert_eq!(m.namespace_for(&bar), Some("inner.example/com"));
    }

    #[test]
    fn build_returns_none_for_file_outside_any_module() {
        let go_mod = PathBuf::from("/repo/go.mod");
        let outside = PathBuf::from("/elsewhere/foo.go");
        let manifests = HashMap::from([(go_mod.clone(), "module github.com/x/y\n")]);
        let m = GoModuleModel::build(vec![go_mod, outside.clone()], closure(manifests));
        assert_eq!(m.namespace_for(&outside), None);
    }

    #[test]
    fn build_skips_malformed_go_mod() {
        let go_mod = PathBuf::from("/repo/go.mod");
        let main_go = PathBuf::from("/repo/main.go");
        // No module directive at all.
        let manifests = HashMap::from([(go_mod.clone(), "go 1.21\n")]);
        let m = GoModuleModel::build(vec![go_mod, main_go.clone()], closure(manifests));
        assert_eq!(m.namespace_for(&main_go), None);
    }
}
