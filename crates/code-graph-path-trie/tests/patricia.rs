//! Patricia split + collapse correctness.
//!
//! These exercise the load-bearing structural invariant: a value-less
//! node never has exactly one child (root excepted). Verified by
//! checking observable behavior — after a sequence of inserts and
//! removes, the trie's `iter()` enumerates exactly the live entries
//! and `contains_path` agrees.

use code_graph_path_trie::PathTrie;
use pretty_assertions::assert_eq;
use std::collections::BTreeSet;
use std::path::PathBuf;

fn collect_paths(t: &PathTrie<u32>) -> BTreeSet<PathBuf> {
    t.paths().collect()
}

#[test]
fn split_at_divergence_point() {
    let mut t: PathTrie<u32> = PathTrie::new();
    t.insert("/a/b/c", 1);
    t.insert("/a/b/d", 2);
    // Should split the existing edge ["/", "a", "b", "c"] at segment 3,
    // producing intermediate /a/b with two leaf children c and d.
    assert_eq!(t.len(), 2);
    assert_eq!(t.get("/a/b/c"), Some(&1));
    assert_eq!(t.get("/a/b/d"), Some(&2));
    assert!(!t.contains_path("/a/b")); // intermediate, no value
}

#[test]
fn split_with_value_at_split_point() {
    let mut t: PathTrie<u32> = PathTrie::new();
    t.insert("/a/b/c", 1);
    t.insert("/a/b", 99); // exact prefix match — value goes on intermediate
    assert_eq!(t.len(), 2);
    assert_eq!(t.get("/a/b"), Some(&99));
    assert_eq!(t.get("/a/b/c"), Some(&1));
}

#[test]
fn descendant_insert_under_existing_value() {
    let mut t: PathTrie<u32> = PathTrie::new();
    t.insert("/a", 1);
    t.insert("/a/b", 2);
    t.insert("/a/b/c", 3);
    assert_eq!(t.len(), 3);
    assert_eq!(t.get("/a"), Some(&1));
    assert_eq!(t.get("/a/b"), Some(&2));
    assert_eq!(t.get("/a/b/c"), Some(&3));
}

#[test]
fn remove_collapses_value_less_single_child_intermediate() {
    // Sequence:
    //   insert /a/b/c        -> single edge ["/", "a", "b", "c"]
    //   insert /a/b/d        -> split into intermediate /a/b
    //   remove /a/b/d        -> intermediate /a/b now value-less + 1 child;
    //                           must collapse so the trie is structurally
    //                           equivalent to the original.
    let mut t: PathTrie<u32> = PathTrie::new();
    t.insert("/a/b/c", 1);
    t.insert("/a/b/d", 2);
    assert_eq!(t.remove("/a/b/d"), Some(2));
    assert_eq!(t.len(), 1);

    // Functional check: get / contains_path / iter all agree.
    assert_eq!(t.get("/a/b/c"), Some(&1));
    assert!(t.contains_path("/a/b/c"));
    assert!(!t.contains_path("/a/b/d"));
    let paths = collect_paths(&t);
    assert_eq!(paths, [PathBuf::from("/a/b/c")].into_iter().collect());

    // Re-insert at /a/b/d to confirm structure still accepts it (would
    // panic or misroute if the invariant were broken).
    t.insert("/a/b/d", 200);
    assert_eq!(t.get("/a/b/d"), Some(&200));
    assert_eq!(t.get("/a/b/c"), Some(&1));
    assert_eq!(t.len(), 2);
}

#[test]
fn remove_chain_collapse_walks_up_until_fork() {
    // Trie shape:
    //
    //   /a/b/c       (value)   <- to remove
    //   /a/x         (value)   <- forces /a to be a fork
    //
    // After removing /a/b/c, the entire /b spine becomes value-less and
    // single-child — collapse must walk up to /a, which has two
    // children (/b and /x). Once /b spine is gone, /a should have only
    // /x as child.
    let mut t: PathTrie<u32> = PathTrie::new();
    t.insert("/a/b/c", 1);
    t.insert("/a/x", 2);

    assert_eq!(t.remove("/a/b/c"), Some(1));
    assert_eq!(t.len(), 1);
    let paths = collect_paths(&t);
    assert_eq!(paths, [PathBuf::from("/a/x")].into_iter().collect());

    // /a/b path should be fully cleared; an insert at /a/b should
    // succeed and not collide with stale nodes.
    t.insert("/a/b", 3);
    assert_eq!(t.get("/a/b"), Some(&3));
    assert_eq!(t.len(), 2);
}

#[test]
fn remove_last_value_leaves_empty_trie() {
    let mut t: PathTrie<u32> = PathTrie::new();
    t.insert("/a/b/c", 1);
    assert_eq!(t.remove("/a/b/c"), Some(1));
    assert!(t.is_empty());
    assert_eq!(t.len(), 0);
    assert!(t.paths().next().is_none());
    // A subsequent insert anywhere works.
    t.insert("/x", 9);
    assert_eq!(t.get("/x"), Some(&9));
}

#[test]
fn remove_intermediate_value_keeps_subtree() {
    // /a/b has value AND /a/b/c has value. Remove /a/b — /a/b/c stays
    // accessible, and the intermediate node either keeps existing
    // (now value-less but still 1+ children) or collapses cleanly.
    let mut t: PathTrie<u32> = PathTrie::new();
    t.insert("/a/b", 10);
    t.insert("/a/b/c", 20);
    assert_eq!(t.remove("/a/b"), Some(10));
    assert_eq!(t.len(), 1);
    assert_eq!(t.get("/a/b/c"), Some(&20));
    assert!(!t.contains_path("/a/b"));
}

#[test]
fn many_inserts_then_removes_stays_consistent() {
    // Hammer the structure: insert N varied paths, remove half in a
    // different order, verify final state matches expectation.
    let inserts = [
        ("/crates/foo/src/lib.rs", 1u32),
        ("/crates/foo/src/main.rs", 2),
        ("/crates/foo/tests/integration.rs", 3),
        ("/crates/bar/src/lib.rs", 4),
        ("/crates/bar/src/util.rs", 5),
        ("/crates/baz/src/lib.rs", 6),
        ("/README.md", 7),
        ("/Cargo.toml", 8),
    ];
    let mut t: PathTrie<u32> = PathTrie::new();
    for &(p, v) in &inserts {
        t.insert(p, v);
    }
    assert_eq!(t.len(), inserts.len());

    let removed = [
        "/crates/foo/src/main.rs",
        "/crates/bar/src/lib.rs",
        "/Cargo.toml",
    ];
    for p in removed {
        assert!(t.remove(p).is_some(), "expected to remove {}", p);
    }
    assert_eq!(t.len(), inserts.len() - removed.len());

    for &(p, v) in &inserts {
        if removed.contains(&p) {
            assert_eq!(t.get(p), None, "{} should be gone", p);
        } else {
            assert_eq!(t.get(p), Some(&v), "{} should still resolve", p);
        }
    }
}
