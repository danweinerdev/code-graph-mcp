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
use tree_sitter::Node;

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
/// sole candidate the target either way. Method references
/// (`String::length`, `obj::method`) and constructor calls also classify
/// `Free` (their capture is not a `method_invocation` name).
pub fn java_call_shape(cap_node: Node<'_>, content: &[u8]) -> CallShape {
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
        return CallShape::Free;
    };
    match receiver.utf8_text(content).ok() {
        Some("this") => CallShape::SelfReceiver,
        _ => CallShape::Receiver,
    }
}
