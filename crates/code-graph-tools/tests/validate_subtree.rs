//! `CodeGraphServer::validate_subtree` scope-validation tests (Finding 5).
//!
//! Before the fix, a relative subtree prefix was normalized against the MCP
//! server's process CWD (via `normalize_user_path`), not the indexed project
//! root, so a client that indexed `/proj` and passed `subtree="src"` (meaning
//! `/proj/src`) would be rejected — or, if the server happened to launch from
//! the project root, accepted only by coincidence. These tests pin that a
//! relative prefix resolves against the indexed root, an absolute in-root
//! prefix still passes, and traversal escapes are rejected.

use code_graph_lang::LanguageRegistry;
use code_graph_lang_cpp::CppParser;
use code_graph_tools::CodeGraphServer;
use tempfile::TempDir;

fn server_with_root(root: std::path::PathBuf) -> CodeGraphServer {
    let mut registry = LanguageRegistry::new();
    registry
        .register(Box::new(CppParser::new().expect("CppParser::new")))
        .unwrap();
    let server = CodeGraphServer::new(registry);
    // validate_subtree only reads inner.root_path; set it directly rather
    // than running a full analyze.
    *server.inner.root_path.write() = Some(root);
    server
}

#[test]
fn relative_subtree_resolves_against_indexed_root() {
    let dir = TempDir::new().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    std::fs::create_dir(root.join("src")).unwrap();
    let server = server_with_root(root.clone());

    let got = server
        .validate_subtree(Some("src"))
        .expect("relative in-root subtree must be accepted");
    let expected = std::fs::canonicalize(root.join("src"))
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert_eq!(
        got,
        Some(expected),
        "relative `src` must resolve to <root>/src, not a CWD-relative path"
    );
}

#[test]
fn absolute_in_root_subtree_still_accepted() {
    let dir = TempDir::new().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    std::fs::create_dir(root.join("src")).unwrap();
    let server = server_with_root(root.clone());

    let abs = root.join("src");
    let got = server
        .validate_subtree(Some(&abs.to_string_lossy()))
        .expect("absolute in-root subtree must be accepted");
    assert_eq!(
        got,
        Some(abs.to_string_lossy().into_owned()),
        "absolute in-root prefix must pass through unchanged"
    );
}

#[test]
fn relative_parent_traversal_rejected() {
    let dir = TempDir::new().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let server = server_with_root(root);

    // `../outside` does not exist on disk; the guard must still reject it
    // (it can't be allowed to slip past the component-wise prefix check).
    let r = server.validate_subtree(Some("../outside"));
    assert!(
        r.is_err(),
        "a `..`-ascending relative subtree must be rejected as outside the root"
    );
}

#[test]
fn absolute_outside_root_rejected() {
    let dir = TempDir::new().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let other = TempDir::new().unwrap();
    let outside = std::fs::canonicalize(other.path()).unwrap();
    let server = server_with_root(root);

    let r = server.validate_subtree(Some(&outside.to_string_lossy()));
    assert!(
        r.is_err(),
        "an absolute path outside the indexed root must be rejected"
    );
}

#[test]
fn absent_and_empty_subtree_resolve_to_none() {
    let dir = TempDir::new().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let server = server_with_root(root);

    assert_eq!(
        server.validate_subtree(None).expect("None is valid"),
        None,
        "absent subtree means no filter"
    );
    assert_eq!(
        server.validate_subtree(Some("")).expect("empty is valid"),
        None,
        "empty subtree means no filter"
    );
}
