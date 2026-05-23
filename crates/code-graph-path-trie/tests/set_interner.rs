//! PathSet + PathInterner.

use code_graph_path_trie::{PathInterner, PathSet};
use pretty_assertions::assert_eq;
use std::path::{Path, PathBuf};

#[test]
fn path_set_insert_contains_remove() {
    let mut s = PathSet::new();
    assert!(s.is_empty());
    assert!(s.insert("/a"));
    assert!(!s.insert("/a")); // duplicate insert returns false
    assert!(s.contains("/a"));
    assert_eq!(s.len(), 1);
    assert!(s.remove("/a"));
    assert!(!s.remove("/a"));
    assert!(s.is_empty());
}

#[test]
fn path_set_subtree_ops() {
    let mut s = PathSet::new();
    s.insert("/crates/foo/src/lib.rs");
    s.insert("/crates/foo/src/main.rs");
    s.insert("/crates/bar/src/lib.rs");

    assert_eq!(s.count_subtree("/crates/foo"), 2);
    assert_eq!(s.count_subtree("/crates"), 3);
    assert_eq!(s.count_subtree("/nowhere"), 0);

    let under_foo: Vec<PathBuf> = s.iter_subtree("/crates/foo").map(|(p, _)| p).collect();
    assert_eq!(under_foo.len(), 2);
}

#[test]
fn path_set_longest_prefix() {
    let mut s = PathSet::new();
    s.insert("/a");
    s.insert("/a/b/c");
    assert_eq!(s.longest_prefix("/a/b/c/d"), Some(PathBuf::from("/a/b/c")));
    assert_eq!(s.longest_prefix("/a/b/x"), Some(PathBuf::from("/a")));
    assert_eq!(s.longest_prefix("/none"), None);
}

#[test]
fn interner_intern_returns_stable_id_for_same_path() {
    let mut i: PathInterner = PathInterner::new();
    let id1 = i.intern("/foo/bar.rs");
    let id2 = i.intern("/foo/bar.rs");
    assert_eq!(id1, id2);
    assert_eq!(i.len(), 1);
}

#[test]
fn interner_distinct_paths_get_distinct_ids() {
    let mut i: PathInterner = PathInterner::new();
    let a = i.intern("/a");
    let b = i.intern("/b");
    let c = i.intern("/c");
    assert_ne!(a, b);
    assert_ne!(b, c);
    assert_ne!(a, c);
    assert_eq!(i.len(), 3);
}

#[test]
fn interner_resolve_returns_original_path() {
    let mut i: PathInterner = PathInterner::new();
    let id = i.intern("/crates/foo/src/lib.rs");
    assert_eq!(i.resolve(id), Some(Path::new("/crates/foo/src/lib.rs")));
}

#[test]
fn interner_get_returns_none_for_unknown_path() {
    let mut i: PathInterner = PathInterner::new();
    i.intern("/known");
    assert_eq!(i.get("/known").map(|id| id.get() > 0), Some(true));
    assert_eq!(i.get("/unknown"), None);
}

#[test]
fn interner_ids_are_dense_and_monotonic() {
    let mut i: PathInterner = PathInterner::new();
    let mut prev = 0u32;
    for n in 0..10 {
        let id = i.intern(format!("/p{}", n));
        let raw = id.get();
        assert_eq!(raw, prev + 1, "ids must be dense + monotonic");
        prev = raw;
    }
    assert_eq!(i.len(), 10);
}
