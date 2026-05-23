//! Insert / get / contains / remove / len round-trip.

use code_graph_path_trie::PathTrie;
use pretty_assertions::assert_eq;
use std::path::PathBuf;

#[test]
fn empty_trie_has_len_zero_and_lookups_return_none() {
    let t: PathTrie<u32> = PathTrie::new();
    assert_eq!(t.len(), 0);
    assert!(t.is_empty());
    assert_eq!(t.get("/foo/bar"), None);
    assert!(!t.contains_path("/foo/bar"));
}

#[test]
fn insert_then_get_round_trips_single_path() {
    let mut t: PathTrie<u32> = PathTrie::new();
    assert_eq!(t.insert("/foo/bar.rs", 42), None);
    assert_eq!(t.get("/foo/bar.rs"), Some(&42));
    assert!(t.contains_path("/foo/bar.rs"));
    assert_eq!(t.len(), 1);
}

#[test]
fn insert_overwrites_and_returns_prior_value() {
    let mut t: PathTrie<&'static str> = PathTrie::new();
    assert_eq!(t.insert("/a", "first"), None);
    assert_eq!(t.insert("/a", "second"), Some("first"));
    assert_eq!(t.get("/a"), Some(&"second"));
    assert_eq!(t.len(), 1);
}

#[test]
fn insert_distinct_paths_each_independent() {
    let mut t: PathTrie<u32> = PathTrie::new();
    t.insert("/a", 1);
    t.insert("/b", 2);
    t.insert("/c", 3);
    assert_eq!(t.len(), 3);
    assert_eq!(t.get("/a"), Some(&1));
    assert_eq!(t.get("/b"), Some(&2));
    assert_eq!(t.get("/c"), Some(&3));
}

#[test]
fn intermediate_directory_is_not_contained_unless_inserted() {
    let mut t: PathTrie<u32> = PathTrie::new();
    t.insert("/a/b/c", 1);
    assert!(t.contains_path("/a/b/c"));
    // /a and /a/b are PURELY intermediate — no value stored, not contained.
    assert!(!t.contains_path("/a"));
    assert!(!t.contains_path("/a/b"));
    assert_eq!(t.get("/a"), None);
    assert_eq!(t.get("/a/b"), None);
}

#[test]
fn insert_at_prefix_then_at_descendant() {
    let mut t: PathTrie<u32> = PathTrie::new();
    t.insert("/a/b", 10);
    t.insert("/a/b/c", 20);
    assert_eq!(t.get("/a/b"), Some(&10));
    assert_eq!(t.get("/a/b/c"), Some(&20));
    assert_eq!(t.len(), 2);
}

#[test]
fn insert_at_descendant_then_at_prefix() {
    let mut t: PathTrie<u32> = PathTrie::new();
    t.insert("/a/b/c", 20);
    t.insert("/a/b", 10);
    assert_eq!(t.get("/a/b"), Some(&10));
    assert_eq!(t.get("/a/b/c"), Some(&20));
    assert_eq!(t.len(), 2);
}

#[test]
fn get_mut_allows_in_place_mutation() {
    let mut t: PathTrie<u32> = PathTrie::new();
    t.insert("/a", 100);
    if let Some(v) = t.get_mut("/a") {
        *v += 23;
    }
    assert_eq!(t.get("/a"), Some(&123));
}

#[test]
fn remove_returns_value_and_decrements_len() {
    let mut t: PathTrie<u32> = PathTrie::new();
    t.insert("/a/b", 10);
    t.insert("/a/c", 20);
    assert_eq!(t.remove("/a/b"), Some(10));
    assert_eq!(t.len(), 1);
    assert!(!t.contains_path("/a/b"));
    assert!(t.contains_path("/a/c"));
}

#[test]
fn remove_missing_path_returns_none_and_keeps_len() {
    let mut t: PathTrie<u32> = PathTrie::new();
    t.insert("/a", 1);
    assert_eq!(t.remove("/nope"), None);
    assert_eq!(t.len(), 1);
}

#[test]
fn remove_intermediate_directory_returns_none() {
    let mut t: PathTrie<u32> = PathTrie::new();
    t.insert("/a/b/c", 1);
    // /a is purely structural; not contained, can't be removed.
    assert_eq!(t.remove("/a"), None);
    assert_eq!(t.len(), 1);
    assert_eq!(t.get("/a/b/c"), Some(&1));
}

#[test]
fn empty_path_targets_root() {
    let mut t: PathTrie<u32> = PathTrie::new();
    assert_eq!(t.insert(PathBuf::new(), 42), None);
    assert_eq!(t.len(), 1);
    assert_eq!(t.get(PathBuf::new()), Some(&42));
    assert!(t.contains_path(PathBuf::new()));
    assert_eq!(t.remove(PathBuf::new()), Some(42));
    assert_eq!(t.len(), 0);
    assert!(t.is_empty());
}

#[test]
fn relative_and_absolute_are_distinct_keys() {
    let mut t: PathTrie<u32> = PathTrie::new();
    t.insert("/foo", 1);
    t.insert("foo", 2);
    assert_eq!(t.get("/foo"), Some(&1));
    assert_eq!(t.get("foo"), Some(&2));
    assert_eq!(t.len(), 2);
}
