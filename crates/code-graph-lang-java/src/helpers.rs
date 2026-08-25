//! Helper routines for the Java parser.
//!
//! Currently empty. The cross-language helpers
//! ([`code_graph_lang::helpers::truncate_signature`],
//! [`code_graph_lang::helpers::find_enclosing_kind`]) are imported
//! directly from `code-graph-lang` at their use sites in `lib.rs` rather
//! than re-exported through this module. The Java-specific helpers
//! (`enclosing_named_type_kind`, `enclosing_named_type_name`,
//! `enclosing_type_name`) live as private functions in `lib.rs` rather
//! than here, because they are tightly coupled to the extractor's
//! tree-walking strategy. This module exists as a per-plugin landing
//! spot for any future Java-specific helpers (e.g. package-path joining,
//! qualified-name flattening for inheritance edges); keeping the file
//! present preserves the same module shape as the C++/Rust/Go/Python/C#
//! plugins so future helpers slot in without churn.

use code_graph_core::CallShape;
use std::collections::HashSet;
use tree_sitter::Node;

/// Static-import bindings visible in one file (phase-12 review,
/// blind-spots F2). `import static a.b.C.foo;` binds `foo` as an
/// unqualified callable whose true target lives OUTSIDE the enclosing
/// class, and `import static a.b.C.*;` binds an unknowable set — so an
/// unqualified call matching one of these must not take the Free
/// sole-candidate shortcut (a sole unrelated indexed `foo` would resolve
/// falsely `Resolved/1`).
pub struct JavaStaticImports {
    names: HashSet<String>,
    wildcard: bool,
}

impl JavaStaticImports {
    pub fn binds(&self, name: &str) -> bool {
        self.wildcard || self.names.contains(name)
    }
}

/// Collect the file's static-import bindings: the simple (last-segment)
/// name of every `import static a.b.C.name;`, plus a wildcard flag for
/// `import static a.b.C.*;`. Java requires imports at top level, so only
/// the root's direct `import_declaration` children are walked. The
/// `static` keyword is an anonymous child of the declaration.
pub fn collect_static_imports(root: Node<'_>, content: &[u8]) -> JavaStaticImports {
    let mut names = HashSet::new();
    let mut wildcard = false;
    let mut cursor = root.walk();
    for decl in root.children(&mut cursor) {
        if decl.kind() != "import_declaration" {
            continue;
        }
        let mut is_static = false;
        let mut has_asterisk = false;
        let mut path_text: Option<&str> = None;
        let mut inner = decl.walk();
        for child in decl.children(&mut inner) {
            match child.kind() {
                "static" => is_static = true,
                "asterisk" => has_asterisk = true,
                "identifier" | "scoped_identifier" if path_text.is_none() => {
                    path_text = child.utf8_text(content).ok();
                }
                _ => {}
            }
        }
        if !is_static {
            continue;
        }
        if has_asterisk {
            wildcard = true;
        } else if let Some(simple) = path_text.and_then(|p| p.rsplit('.').next()) {
            if !simple.is_empty() {
                names.insert(simple.to_owned());
            }
        }
    }
    JavaStaticImports { names, wildcard }
}

/// Classify a captured callee identifier's call shape (F2).
///
/// `cap_node` is the `call.name` capture. When it is the `name` child of a
/// `method_invocation` that HAS an `object` child, the call goes through a
/// receiver: `this.foo()` is [`CallShape::SelfReceiver`] (the receiver type
/// is the enclosing class), anything else — including `super.foo()` (the
/// generic resolver cannot verify the base chain), package/type-qualified
/// static calls (`Ns.Type.method()`, indistinguishable from a field access
/// chain without semantic info), and chained calls — is
/// [`CallShape::Receiver`]. An unqualified invocation (no `object` child:
/// implicit-`this` or static import) stays [`CallShape::Free`] — same
/// decision as C++'s implicit-`this` note in KNOWN_ISSUES F2: with exactly
/// one indexed candidate, member-if-exists-else-global lookup makes the
/// sole candidate the target either way. EXCEPTION (phase-12 review,
/// blind-spots F2): an unqualified call whose name is bound by a static
/// import (`import static a.b.C.foo; foo();`) — or any unqualified call
/// under a wildcard static import — classifies as
/// [`CallShape::Receiver`], because the import declares the true target
/// lives outside the enclosing class and a sole unrelated indexed
/// candidate would otherwise resolve falsely `Resolved/1`. Method
/// references (`String::length`, `obj::method`) and constructor calls
/// classify `Free` (their capture is not a `method_invocation` name).
pub fn java_call_shape(
    cap_node: Node<'_>,
    content: &[u8],
    static_imports: &JavaStaticImports,
) -> CallShape {
    let Some(parent) = cap_node.parent() else {
        return CallShape::Free;
    };
    if parent.kind() != "method_invocation" {
        return CallShape::Free;
    }
    if parent.child_by_field_name("name").map(|n| n.id()) != Some(cap_node.id()) {
        return CallShape::Free;
    }
    let Some(receiver) = parent.child_by_field_name("object") else {
        let bound_by_static_import = cap_node
            .utf8_text(content)
            .is_ok_and(|name| static_imports.binds(name));
        return if bound_by_static_import {
            CallShape::Receiver
        } else {
            CallShape::Free
        };
    };
    match receiver.utf8_text(content).ok() {
        Some("this") => CallShape::SelfReceiver,
        _ => CallShape::Receiver,
    }
}
