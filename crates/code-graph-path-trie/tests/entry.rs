//! Entry API: or_insert / or_insert_with / or_default / and_modify.

use code_graph_path_trie::{Entry, PathTrie};
use pretty_assertions::assert_eq;

#[test]
fn or_insert_returns_existing_value() {
    let mut t: PathTrie<u32> = PathTrie::new();
    t.insert("/a", 10);
    let v = t.entry("/a").or_insert(99);
    assert_eq!(*v, 10);
    assert_eq!(t.get("/a"), Some(&10));
}

#[test]
fn or_insert_inserts_when_absent() {
    let mut t: PathTrie<u32> = PathTrie::new();
    let v = t.entry("/a/b").or_insert(42);
    assert_eq!(*v, 42);
    assert_eq!(t.get("/a/b"), Some(&42));
    assert_eq!(t.len(), 1);
}

#[test]
fn or_insert_with_only_called_on_vacant() {
    let mut t: PathTrie<u32> = PathTrie::new();
    let v = t.entry("/x").or_insert_with(|| 100);
    *v += 1;
    let v = t
        .entry("/x")
        .or_insert_with(|| panic!("should not be called"));
    assert_eq!(*v, 101);
}

#[test]
fn or_default_creates_default_value() {
    let mut t: PathTrie<String> = PathTrie::new();
    let s = t.entry("/path").or_default();
    s.push_str("hello");
    assert_eq!(t.get("/path"), Some(&"hello".to_string()));
}

#[test]
fn and_modify_on_occupied_runs_closure() {
    let mut t: PathTrie<u32> = PathTrie::new();
    t.insert("/a", 5);
    let v = t.entry("/a").and_modify(|v| *v *= 10).or_insert(0);
    assert_eq!(*v, 50);
}

#[test]
fn and_modify_on_vacant_does_not_call_closure() {
    let mut t: PathTrie<u32> = PathTrie::new();
    let v = t
        .entry("/a")
        .and_modify(|_| panic!("should not run on vacant"))
        .or_insert(7);
    assert_eq!(*v, 7);
}

#[test]
fn matching_on_entry_variant() {
    let mut t: PathTrie<u32> = PathTrie::new();
    t.insert("/exists", 1);
    match t.entry("/exists") {
        Entry::Occupied(e) => assert_eq!(*e.get(), 1),
        Entry::Vacant(_) => panic!("expected Occupied"),
    }
    match t.entry("/missing") {
        Entry::Vacant(_) => {}
        Entry::Occupied(_) => panic!("expected Vacant"),
    }
}
