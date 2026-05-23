//! Iteration: order, subtree, ancestors, longest_prefix,
//! for_each_subtree_mut, count_subtree.

use code_graph_path_trie::PathTrie;
use pretty_assertions::assert_eq;
use std::path::PathBuf;

fn paths_of<V>(it: impl IntoIterator<Item = (PathBuf, V)>) -> Vec<PathBuf> {
    it.into_iter().map(|(p, _)| p).collect()
}

#[test]
fn iter_yields_lex_sorted_paths() {
    let mut t: PathTrie<u32> = PathTrie::new();
    t.insert("/zeta", 1);
    t.insert("/alpha", 2);
    t.insert("/mu", 3);
    let got = paths_of(t.iter().map(|(p, v)| (p, *v)));
    assert_eq!(
        got,
        vec![
            PathBuf::from("/alpha"),
            PathBuf::from("/mu"),
            PathBuf::from("/zeta"),
        ]
    );
}

#[test]
fn iter_visits_intermediate_value_before_descendant() {
    let mut t: PathTrie<u32> = PathTrie::new();
    t.insert("/a", 1);
    t.insert("/a/b", 2);
    t.insert("/a/b/c", 3);
    let got = paths_of(t.iter().map(|(p, v)| (p, *v)));
    assert_eq!(
        got,
        vec![
            PathBuf::from("/a"),
            PathBuf::from("/a/b"),
            PathBuf::from("/a/b/c"),
        ]
    );
}

#[test]
fn iter_skips_value_less_intermediate_nodes() {
    let mut t: PathTrie<u32> = PathTrie::new();
    t.insert("/a/b/c", 1);
    t.insert("/a/b/d", 2);
    let got: Vec<(PathBuf, u32)> = t.iter().map(|(p, v)| (p, *v)).collect();
    assert_eq!(got.len(), 2);
    assert_eq!(got[0].0, PathBuf::from("/a/b/c"));
    assert_eq!(got[1].0, PathBuf::from("/a/b/d"));
}

#[test]
fn paths_and_values_match_iter() {
    let mut t: PathTrie<u32> = PathTrie::new();
    t.insert("/a", 10);
    t.insert("/b", 20);
    t.insert("/c", 30);

    let from_paths: Vec<PathBuf> = t.paths().collect();
    let from_values: Vec<u32> = t.values().copied().collect();
    let from_iter: Vec<(PathBuf, u32)> = t.iter().map(|(p, v)| (p, *v)).collect();

    assert_eq!(
        from_paths,
        vec![
            PathBuf::from("/a"),
            PathBuf::from("/b"),
            PathBuf::from("/c"),
        ]
    );
    assert_eq!(from_values, vec![10, 20, 30]);
    assert_eq!(from_iter.len(), 3);
}

#[test]
fn iter_subtree_includes_prefix_value() {
    let mut t: PathTrie<u32> = PathTrie::new();
    t.insert("/a", 1);
    t.insert("/a/b", 2);
    t.insert("/a/b/c", 3);
    t.insert("/other", 99);

    let got: Vec<(PathBuf, u32)> = t.iter_subtree("/a").map(|(p, v)| (p, *v)).collect();
    let paths: Vec<PathBuf> = got.iter().map(|(p, _)| p.clone()).collect();
    assert_eq!(
        paths,
        vec![
            PathBuf::from("/a"),
            PathBuf::from("/a/b"),
            PathBuf::from("/a/b/c"),
        ]
    );
    // /other excluded.
    assert!(!paths.contains(&PathBuf::from("/other")));
}

#[test]
fn iter_subtree_returns_empty_when_prefix_absent() {
    let t: PathTrie<u32> = PathTrie::new();
    assert!(t.iter_subtree("/nothing").next().is_none());
}

#[test]
fn iter_subtree_returns_empty_when_prefix_diverges() {
    let mut t: PathTrie<u32> = PathTrie::new();
    t.insert("/a/b/c", 1);
    // /a/b/d doesn't exist as a node — Patricia edge ["/", "a", "b", "c"]
    // doesn't match d at index 3.
    assert!(t.iter_subtree("/a/b/d").next().is_none());
}

#[test]
fn count_subtree_matches_iter_count() {
    let mut t: PathTrie<u32> = PathTrie::new();
    t.insert("/a/x", 1);
    t.insert("/a/y", 2);
    t.insert("/a/z/w", 3);
    t.insert("/b", 99);

    assert_eq!(t.count_subtree("/a"), 3);
    assert_eq!(t.count_subtree("/"), 4);
    assert_eq!(t.count_subtree("/nothing"), 0);

    // Cross-check: iter_subtree yields the same count.
    assert_eq!(t.iter_subtree("/a").count(), t.count_subtree("/a"));
}

#[test]
fn longest_prefix_finds_deepest_valued_ancestor() {
    let mut t: PathTrie<u32> = PathTrie::new();
    t.insert("/a", 1);
    t.insert("/a/b", 2);
    t.insert("/a/b/c", 3);

    let (p, v) = t.longest_prefix("/a/b/c/d/e").unwrap();
    assert_eq!(p, PathBuf::from("/a/b/c"));
    assert_eq!(*v, 3);

    // Query that lands between values.
    let (p, v) = t.longest_prefix("/a/b/x").unwrap();
    assert_eq!(p, PathBuf::from("/a/b"));
    assert_eq!(*v, 2);

    // Query that misses entirely.
    assert!(t.longest_prefix("/nope").is_none());
}

#[test]
fn longest_prefix_ignores_intermediate_only_nodes() {
    let mut t: PathTrie<u32> = PathTrie::new();
    t.insert("/a/b/c", 1);
    t.insert("/a/b/d", 2);
    // /a/b is intermediate only (no value).
    assert!(t.longest_prefix("/a/b/whatever").is_none());
}

#[test]
fn iter_ancestors_root_down_skipping_unvalued() {
    let mut t: PathTrie<u32> = PathTrie::new();
    t.insert("/a", 1);
    t.insert("/a/b", 2);
    t.insert("/a/b/c", 3);

    let got: Vec<(PathBuf, u32)> = t
        .iter_ancestors("/a/b/c/d/e")
        .map(|(p, v)| (p, *v))
        .collect();
    assert_eq!(
        got,
        vec![
            (PathBuf::from("/a"), 1),
            (PathBuf::from("/a/b"), 2),
            (PathBuf::from("/a/b/c"), 3),
        ]
    );
}

#[test]
fn for_each_subtree_mut_mutates_in_place() {
    let mut t: PathTrie<u32> = PathTrie::new();
    t.insert("/a/x", 10);
    t.insert("/a/y", 20);
    t.insert("/b/z", 30); // outside the subtree

    t.for_each_subtree_mut("/a", |_p, v| *v *= 2);

    assert_eq!(t.get("/a/x"), Some(&20));
    assert_eq!(t.get("/a/y"), Some(&40));
    assert_eq!(t.get("/b/z"), Some(&30)); // unchanged
}

#[test]
fn remove_subtree_drops_descendants_and_returns_pairs() {
    let mut t: PathTrie<u32> = PathTrie::new();
    t.insert("/a/x", 1);
    t.insert("/a/y", 2);
    t.insert("/a/z/w", 3);
    t.insert("/b", 99);

    let mut dropped = t.remove_subtree("/a");
    dropped.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        dropped,
        vec![
            (PathBuf::from("/a/x"), 1),
            (PathBuf::from("/a/y"), 2),
            (PathBuf::from("/a/z/w"), 3),
        ]
    );
    assert_eq!(t.len(), 1);
    assert!(t.contains_path("/b"));
    assert!(!t.contains_path("/a/x"));
}

#[test]
fn remove_subtree_with_prefix_inside_patricia_edge() {
    // /a/b/c is the only entry. Edge label is ["/", "a", "b", "c"].
    // remove_subtree("/a/b") lands inside the edge — should drop the
    // whole subtree anyway.
    let mut t: PathTrie<u32> = PathTrie::new();
    t.insert("/a/b/c", 1);
    let dropped = t.remove_subtree("/a/b");
    assert_eq!(dropped, vec![(PathBuf::from("/a/b/c"), 1)]);
    assert_eq!(t.len(), 0);
}

#[test]
fn remove_subtree_root_clears_everything() {
    let mut t: PathTrie<u32> = PathTrie::new();
    t.insert("/a", 1);
    t.insert("/b/c", 2);
    t.insert("/d", 3);
    let dropped = t.remove_subtree("");
    assert_eq!(dropped.len(), 3);
    assert_eq!(t.len(), 0);
    assert!(t.is_empty());
}
