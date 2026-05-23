//! Oracle stress test: a deterministic PRNG generates a workload of
//! mixed insert/remove/get/longest_prefix/count_subtree operations
//! against both [`PathTrie`] and a [`BTreeMap`] reference. After every
//! op the two are asserted equivalent.
//!
//! Catches algorithmic bugs that hand-written tests miss: e.g. a
//! Patricia split that leaks a stale child, or a collapse that detaches
//! the wrong subtree.

use code_graph_path_trie::PathTrie;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Tiny linear-congruential PRNG — deterministic, no deps.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed)
    }
    fn next_u32(&mut self) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 32) as u32
    }
    fn range(&mut self, n: u32) -> u32 {
        self.next_u32() % n
    }
    fn choose<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.range(items.len() as u32) as usize]
    }
}

/// A small universe of plausible code-paths drawn from real codebase shapes.
const UNIVERSE: &[&str] = &[
    "/crates/foo/src/lib.rs",
    "/crates/foo/src/main.rs",
    "/crates/foo/src/util/mod.rs",
    "/crates/foo/src/util/parse.rs",
    "/crates/foo/tests/it.rs",
    "/crates/bar/src/lib.rs",
    "/crates/bar/src/handlers.rs",
    "/crates/bar/src/handlers/auth.rs",
    "/crates/bar/src/handlers/users.rs",
    "/crates/baz/Cargo.toml",
    "/crates/baz/src/lib.rs",
    "/crates/baz/src/lib/a.rs",
    "/crates/baz/src/lib/b.rs",
    "/crates/baz/src/lib/a/x.rs",
    "/crates/baz/src/lib/a/y.rs",
    "/Cargo.toml",
    "/README.md",
    "/Makefile",
    "/.gitignore",
    "/external/llvm/lib/Foo.cpp",
    "/external/llvm/lib/Foo.h",
    "/external/llvm/include/Foo.h",
    "/a", // intentionally short single-segment
    "/a/b",
    "/a/b/c",
    "/a/b/c/d",
];

fn assert_equivalent(trie: &PathTrie<u32>, oracle: &BTreeMap<PathBuf, u32>) {
    assert_eq!(
        trie.len(),
        oracle.len(),
        "len divergence: trie {} vs oracle {}",
        trie.len(),
        oracle.len()
    );

    for (k, v) in oracle {
        assert_eq!(
            trie.get(k),
            Some(v),
            "trie.get({:?}) should be Some({}) per oracle",
            k,
            v
        );
        assert!(trie.contains_path(k), "trie should contain {:?}", k);
    }

    // The opposite: every trie path is in the oracle, with matching value.
    let trie_paths: BTreeMap<PathBuf, u32> = trie.iter().map(|(p, v)| (p, *v)).collect();
    assert_eq!(trie_paths.len(), oracle.len(), "iter set divergence");
    for (k, v) in &trie_paths {
        assert_eq!(
            oracle.get(k),
            Some(v),
            "iter yielded {:?}={}, oracle differs",
            k,
            v
        );
    }
}

fn longest_prefix_oracle<'a>(
    oracle: &'a BTreeMap<PathBuf, u32>,
    path: &Path,
) -> Option<(PathBuf, &'a u32)> {
    let mut best: Option<(PathBuf, &u32)> = None;
    for (k, v) in oracle {
        if path.starts_with(k) {
            // starts_with is segment-aware (it does not match partial segments),
            // matching the trie's semantic exactly.
            best = match best {
                None => Some((k.clone(), v)),
                Some((cur, _)) if k.components().count() > cur.components().count() => {
                    Some((k.clone(), v))
                }
                other => other,
            };
        }
    }
    best
}

fn count_subtree_oracle(oracle: &BTreeMap<PathBuf, u32>, prefix: &Path) -> usize {
    if prefix.as_os_str().is_empty() {
        return oracle.len();
    }
    oracle.keys().filter(|k| k.starts_with(prefix)).count()
}

#[test]
fn oracle_stress_seed_1() {
    run_stress(1, 2000);
}

#[test]
fn oracle_stress_seed_42() {
    run_stress(42, 2000);
}

#[test]
fn oracle_stress_seed_7919() {
    run_stress(7919, 5000);
}

fn run_stress(seed: u64, ops: usize) {
    let mut rng = Rng::new(seed);
    let mut trie: PathTrie<u32> = PathTrie::new();
    let mut oracle: BTreeMap<PathBuf, u32> = BTreeMap::new();

    for step in 0..ops {
        let op = rng.range(6);
        let path = PathBuf::from(*rng.choose(UNIVERSE));
        let val = rng.next_u32();

        match op {
            // Insert (40% — biased to keep things populated)
            0 | 1 => {
                let prev_trie = trie.insert(&path, val);
                let prev_oracle = oracle.insert(path.clone(), val);
                assert_eq!(
                    prev_trie, prev_oracle,
                    "step {} insert {:?}: prior mismatch",
                    step, path
                );
            }
            // Remove
            2 => {
                let r_trie = trie.remove(&path);
                let r_oracle = oracle.remove(&path);
                assert_eq!(
                    r_trie, r_oracle,
                    "step {} remove {:?}: outcome mismatch",
                    step, path
                );
            }
            // Get
            3 => {
                assert_eq!(
                    trie.get(&path),
                    oracle.get(&path),
                    "step {} get {:?}",
                    step,
                    path
                );
            }
            // longest_prefix
            4 => {
                let t = trie.longest_prefix(&path);
                let o = longest_prefix_oracle(&oracle, &path);
                assert_eq!(
                    t.map(|(p, v)| (p, *v)),
                    o.map(|(p, v)| (p, *v)),
                    "step {} longest_prefix {:?}",
                    step,
                    path
                );
            }
            // count_subtree
            5 => {
                assert_eq!(
                    trie.count_subtree(&path),
                    count_subtree_oracle(&oracle, &path),
                    "step {} count_subtree {:?}",
                    step,
                    path
                );
            }
            _ => unreachable!(),
        }

        // Cheap invariant after every op.
        assert_eq!(trie.len(), oracle.len(), "step {} len divergence", step);
    }

    // Full equivalence check at the end.
    assert_equivalent(&trie, &oracle);
}

#[test]
fn oracle_remove_subtree_matches_filter() {
    // remove_subtree across a few seeds; the dropped set should equal
    // the oracle's keys-starting-with-prefix filter, both as a set.
    use std::collections::BTreeSet;
    for &seed in &[1u64, 13, 99] {
        let mut rng = Rng::new(seed);
        let mut trie: PathTrie<u32> = PathTrie::new();
        let mut oracle: BTreeMap<PathBuf, u32> = BTreeMap::new();

        // Populate.
        for _ in 0..50 {
            let path = PathBuf::from(*rng.choose(UNIVERSE));
            let val = rng.next_u32();
            trie.insert(&path, val);
            oracle.insert(path, val);
        }

        // Pick a prefix and drop.
        let prefix = PathBuf::from(*rng.choose(&[
            "/crates/foo",
            "/crates/bar",
            "/crates/baz",
            "/external",
            "/a/b",
        ]));

        let dropped_trie: BTreeSet<PathBuf> = trie
            .remove_subtree(&prefix)
            .into_iter()
            .map(|(p, _)| p)
            .collect();
        let dropped_oracle: BTreeSet<PathBuf> = oracle
            .keys()
            .filter(|k| k.starts_with(&prefix))
            .cloned()
            .collect();
        for k in &dropped_oracle {
            oracle.remove(k);
        }

        assert_eq!(
            dropped_trie, dropped_oracle,
            "seed {}, prefix {:?}: dropped set mismatch",
            seed, prefix
        );
        assert_equivalent(&trie, &oracle);
    }
}
