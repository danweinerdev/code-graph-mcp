//! C++ parser regression tests for API-macro / reflection-annotation /
//! inheritance / cascade-leak / macro-expansion shapes commonly found in
//! large engine-style C++ codebases.
//!
//! Each test mirrors a TC-XX case from an internal field report capturing
//! parser-behaviour failures in a real-world C++ tree on 2026-05-22. The
//! fixtures here are SYNTHESIZED: all macro names, type names, method
//! names, and file paths are generic placeholders chosen to preserve the
//! exact syntactic shape that triggers each failure mode (export macros
//! between `class` and the identifier, parenthesized annotation macros
//! above class declarations, inline forward declarations inside parameter
//! lists, template-instantiation base classes, function-defining
//! `#define`s with `##` token-pasting, etc.) without carrying any
//! third-party or proprietary identifiers.
//!
//! ## Important caveat — minimal vs. real-world reproduction
//!
//! The original report's PRIMARY claim — that `class <MACRO>_API Foo { … };`
//! fails to extract on real headers despite the macro being in
//! `[cpp].macro_strip` — does NOT reproduce against minimal synthetic
//! fixtures. The parser correctly indexes the class for every shape we
//! distilled here. That tells us one of two things:
//!
//! 1. The failure depends on additional context in the real header
//!    (file size, preceding include depth, specific preprocessor state,
//!    nested macros, conditional compilation) that our minimal fixtures
//!    don't reconstruct.
//! 2. The user's `.code-graph.toml` was not actually being loaded for
//!    the indexed root (a configuration-discovery issue rather than a
//!    parser issue).
//!
//! The tests below therefore have a dual role:
//! - **Guard the working baseline.** Every minimal shape that the parser
//!   handles correctly today gets a passing test so a future change that
//!   regresses it gets caught immediately.
//! - **Document the genuine gaps.** Where a minimal fixture DOES fail
//!   (template-instantiation inheritance in TC-B1; virtual destructor in
//!   TC-A4 — both pre-existing limitations not introduced by the field
//!   report), the test asserts the EXPECTED behavior and carries
//!   `#[ignore = "TC-XX: …"]` so CI stays green. Remove the `#[ignore]`
//!   when the fix lands.
//!
//! If a future investigation captures a sliced-down anonymised real
//! header that DOES reproduce the primary failure, add it as a fixture
//! file under `tests/fixtures/parser_bug_<name>.h` and write a
//! fixture-driven test alongside the synthetic ones here.
//!
//! ## Categories (matching the field report's TC-XX taxonomy)
//! - **A** — macro-strip integration failures (P0; the report's headline)
//! - **B** — inheritance graph gaps (P1–P2)
//! - **C** — call graph resolution noise (P1–P2)
//!
//! Categories D (search UX), E (bulk-analysis), and F (operational) live
//! outside the C++ parser layer and are out of scope for this file.

use std::path::Path;

use code_graph_core::{EdgeKind, FileGraph, RootConfig, Symbol, SymbolKind};
use code_graph_lang::LanguagePlugin;
use code_graph_lang_cpp::CppParser;

// --- helpers --------------------------------------------------------------

/// Parse `src` after running `CppParser::preprocess` with the given
/// `macro_strip` and `macro_strip_with_args` lists. Mirrors the full
/// production pipeline so the test exercises the same code path as
/// `analyze_codebase` against a `.code-graph.toml`-configured tree.
fn parse_with_strip(src: &str, macros: &[&str], with_args: &[&str]) -> FileGraph {
    let mut cfg = RootConfig::default();
    cfg.cpp.macro_strip = macros.iter().map(|s| s.to_string()).collect();
    cfg.cpp.macro_strip_with_args = with_args.iter().map(|s| s.to_string()).collect();
    let p = CppParser::new().expect("CppParser::new must succeed");
    let cleaned = p.preprocess(src.as_bytes(), &cfg);
    p.parse_file(Path::new("/test.h"), &cleaned)
        .expect("parse_file must succeed")
}

fn find_symbol<'a>(fg: &'a FileGraph, name: &str) -> Option<&'a Symbol> {
    fg.symbols.iter().find(|s| s.name == name)
}

fn find_method<'a>(fg: &'a FileGraph, parent: &str, name: &str) -> Option<&'a Symbol> {
    fg.symbols
        .iter()
        .find(|s| s.name == name && s.parent == parent)
}

fn has_inherits_edge(fg: &FileGraph, derived: &str, base: &str) -> bool {
    fg.edges
        .iter()
        .any(|e| e.kind == EdgeKind::Inherits && e.from == derived && e.to == base)
}

// =========================================================================
// Category A — macro-strip integration baseline + cascade guards
// =========================================================================

/// TC-A1 (control) — a class declared WITHOUT an API macro, in the same
/// source as a `MYLIB_API`-prefixed sibling, must continue to index
/// correctly. The original report used this as its A/B baseline: same
/// scope, same surrounding code, only the API macro differs — so a fix
/// for the failing half cannot regress this control case.
#[test]
fn tc_a1_control_class_without_api_macro_indexes() {
    let src = "\
class MYLIB_API MyString {};
class MyStringWriter : public MyString, public MyWriter {};
";
    let fg = parse_with_strip(src, &["MYLIB_API"], &[]);

    let s = find_symbol(&fg, "MyStringWriter")
        .expect("MyStringWriter (no API macro) must index — control case");
    assert_eq!(s.kind, SymbolKind::Class);
    assert!(
        has_inherits_edge(&fg, "MyStringWriter", "MyString"),
        "MyStringWriter -> MyString inherits edge must exist"
    );
    assert!(
        has_inherits_edge(&fg, "MyStringWriter", "MyWriter"),
        "MyStringWriter -> MyWriter inherits edge must exist"
    );
}

/// TC-A1 (subject) — `class MYLIB_API MyString { … };` with in-class
/// constructors and a const method declaration. The report cited an
/// analogous real-world header where this shape failed to extract the
/// class and instead surfaced an in-class constructor as a top-level
/// free function bearing the class's name.
///
/// In minimal form: PASSES today. Pinned so a regression is caught
/// immediately. If a real-file failure ever gets isolated, add a fixture
/// file alongside this test.
#[test]
fn tc_a1_class_with_export_macro_indexes() {
    let src = "\
class MYLIB_API MyString
{
public:
    MyString();
    MyString(const char* In);
    int Len() const;
};
";
    let fg = parse_with_strip(src, &["MYLIB_API"], &[]);

    let s = find_symbol(&fg, "MyString").expect("MyString class must be indexed");
    assert_eq!(
        s.kind,
        SymbolKind::Class,
        "MyString must be a Class, not a Function"
    );

    // Forward declarations (the three method declarations above) are
    // deliberately excluded from the symbol set — see CLAUDE.md C++
    // limitations §5. Guard that as a positive invariant: no top-level
    // free function named `MyString` may surface, regardless of whether
    // forward decls were collected.
    let leaked_ctor = fg
        .symbols
        .iter()
        .find(|s| s.name == "MyString" && s.kind != SymbolKind::Class && s.parent.is_empty());
    assert!(
        leaked_ctor.is_none(),
        "MyString constructor must NOT surface as a top-level free function; \
         got {leaked_ctor:?}"
    );
}

/// TC-A2 — `class OTHERLIB_API DerivedThing : public BaseThing { … };`.
/// API macro plus inheritance. The `Inherits` edge must survive macro
/// stripping. Forward-declared methods inside the class are intentionally
/// excluded from the symbol set per the C++ parser's design.
#[test]
fn tc_a2_api_macro_plus_inheritance_emits_inherits_edge() {
    let src = "\
class BaseThing {};
class OTHERLIB_API DerivedThing : public BaseThing
{
public:
    void Tick();
};
";
    let fg = parse_with_strip(src, &["OTHERLIB_API"], &[]);

    assert_eq!(
        find_symbol(&fg, "BaseThing").map(|s| s.kind),
        Some(SymbolKind::Class)
    );
    assert_eq!(
        find_symbol(&fg, "DerivedThing").map(|s| s.kind),
        Some(SymbolKind::Class),
        "DerivedThing (OTHERLIB_API stripped) must index as Class"
    );
    assert!(
        has_inherits_edge(&fg, "DerivedThing", "BaseThing"),
        "DerivedThing -> BaseThing inherits edge must exist after OTHERLIB_API strip"
    );
}

/// TC-A3 — a fully-loaded "reflection-annotated entity" shape: a
/// parenthesized annotation macro with nested parens + quoted strings
/// above `class OTHERLIB_API DerivedEntity : public BaseEntity { … };`,
/// with a body-opening reflection macro on the first line. Stresses
/// (a) the API macro, (b) the parenthesized annotation's argument-aware
/// stripping, (c) the body-opening macro inside a class body — all
/// three composing on the same declaration.
///
/// Minimal form: PASSES today; pinned as regression guard.
#[test]
fn tc_a3_annotation_plus_api_macro_plus_body_macro() {
    let src = r#"
class BaseEntity {};

ANNOTATED(Trait, Configurable, config=Library, meta=(Tooltip="A test entity with annotations and a quoted description."))
class OTHERLIB_API DerivedEntity : public BaseEntity
{
    REGISTER_REFLECTION()

public:
    DerivedEntity();
    void BeginPlay();
};
"#;
    let fg = parse_with_strip(
        src,
        &["OTHERLIB_API"],
        &["ANNOTATED", "REGISTER_REFLECTION"],
    );

    let s = find_symbol(&fg, "DerivedEntity").expect("DerivedEntity must be indexed as a class");
    assert_eq!(s.kind, SymbolKind::Class);

    assert!(
        has_inherits_edge(&fg, "DerivedEntity", "BaseEntity"),
        "DerivedEntity -> BaseEntity inherits edge must exist"
    );

    // Constructor must not leak as a top-level free function.
    let leaked_ctor = fg
        .symbols
        .iter()
        .find(|s| s.name == "DerivedEntity" && s.kind != SymbolKind::Class && s.parent.is_empty());
    assert!(
        leaked_ctor.is_none(),
        "DerivedEntity constructor must NOT surface as a top-level free function; got {leaked_ctor:?}"
    );
}

/// TC-A4 — the smallest "minimal failing real-world shape" pattern.
/// `class THIRDLIB_API MyHook { … };` with virtual destructor, virtual
/// method declarations, and inline `class MyEditChain*` forward
/// declarations inside parameter lists. No nested types, no `#if`, no
/// annotation macros — just one export macro between `class` and the
/// identifier, plus the messy parameter shapes typical of engine-style
/// callback interfaces.
///
/// Minimal form: PASSES today (class indexes, in-body methods attribute
/// to the class). Pinned as regression guard.
#[test]
fn tc_a4_minimal_real_world_shape_with_inline_forward_decls() {
    let src = "\
class THIRDLIB_API MyHook
{
public:
    virtual ~MyHook() {}
    virtual void NotifyPreChange( MyProperty* PropertyAboutToChange ) {}
    virtual void NotifyPreChange( class MyEditChain* PropertyAboutToChange );
    virtual void NotifyPostChange( const MyChangeEvent& Event, MyProperty* PropertyThatChanged ) {}
    virtual void NotifyPostChange( const MyChangeEvent& Event, class MyEditChain* PropertyThatChanged );
};
";
    let fg = parse_with_strip(src, &["THIRDLIB_API"], &[]);

    let s =
        find_symbol(&fg, "MyHook").expect("MyHook must index — minimal failing real-world shape");
    assert_eq!(s.kind, SymbolKind::Class);

    // Defined methods (those with bodies) must attribute to the class.
    // Forward-declared methods on subsequent lines are intentionally
    // excluded per parser design.
    assert!(
        find_method(&fg, "MyHook", "NotifyPreChange").is_some(),
        "MyHook::NotifyPreChange (defined) must attribute to MyHook"
    );
    assert!(
        find_method(&fg, "MyHook", "NotifyPostChange").is_some(),
        "MyHook::NotifyPostChange (defined) must attribute to MyHook"
    );

    // The `class MyEditChain` inline forward declarations inside
    // parameter lists must NOT surface as standalone Symbols.
    let leaked_inline_fwd = fg.symbols.iter().find(|s| s.name == "MyEditChain");
    assert!(
        leaked_inline_fwd.is_none(),
        "inline `class MyEditChain*` in a parameter list must NOT emit a Symbol; \
         got {leaked_inline_fwd:?}"
    );
}

/// TC-A4 destructor gap — virtual destructor with body
/// (`virtual ~MyHook() {}`) is NOT emitted as a Symbol today.
/// Pre-existing parser gap, separate from the field report's primary
/// claim but uncovered while building this suite.
#[test]
#[ignore = "TC-A4 destructor: parser does not emit a Symbol for `virtual ~Foo() {}`. \
            Pre-existing limitation; flip on when destructor extraction is added."]
fn tc_a4_destructor_with_body_emits_method_symbol() {
    let src = "\
class THIRDLIB_API MyHook
{
public:
    virtual ~MyHook() {}
};
";
    let fg = parse_with_strip(src, &["THIRDLIB_API"], &[]);

    // Either `~MyHook` or `MyHook` (mirroring the constructor
    // convention) as a method with `parent = MyHook` would satisfy
    // this; the parser currently emits neither.
    let dtor = fg.symbols.iter().find(|s| {
        (s.name == "~MyHook" || s.name == "MyHook")
            && s.parent == "MyHook"
            && matches!(s.kind, SymbolKind::Method | SymbolKind::Function)
    });
    assert!(
        dtor.is_some(),
        "virtual destructor `~MyHook() {{}}` must attribute to MyHook"
    );
}

/// TC-A5 (cascade) — when the outer class fails to parse, the original
/// report observed: inner non-macro structs index correctly but the
/// outer class's forward-declared methods leak as top-level free
/// functions.
///
/// Minimal form: PASSES today. Outer class indexes, inner struct
/// attributes to outer as parent, forward-declared outer methods do
/// not leak. Pinned as regression guard for the cascade pattern.
#[test]
fn tc_a5_outer_class_methods_dont_leak_as_top_level() {
    let src = "\
class THIRDLIB_API OuterCollector
{
private:
    struct InnerProperty
    {
        InnerProperty(int A, int B, bool C);
        bool operator==(const InnerProperty& Other) const { return true; }
    };

public:
    bool HasAnyPendingItems() const;
    void ResolveReference(int A = 0, bool B = true);
};
";
    let fg = parse_with_strip(src, &["THIRDLIB_API"], &[]);

    // Outer class indexes.
    assert_eq!(
        find_symbol(&fg, "OuterCollector").map(|s| s.kind),
        Some(SymbolKind::Class),
        "OuterCollector outer class must index"
    );

    // Inner struct attributes to the outer class.
    let inner = find_symbol(&fg, "InnerProperty").expect("inner struct InnerProperty must index");
    assert_eq!(
        inner.parent, "OuterCollector",
        "inner struct's parent must be the outer class"
    );

    // operator== inside the inner struct attributes to the inner struct.
    assert!(
        find_method(&fg, "InnerProperty", "operator==").is_some(),
        "InnerProperty::operator== must attribute to the inner struct"
    );

    // Outer-class forward-declared methods must NOT leak as top-level
    // free functions. This is the specific cascade the report flagged.
    let leaked_has = fg
        .symbols
        .iter()
        .find(|s| s.name == "HasAnyPendingItems" && s.parent.is_empty());
    let leaked_resolve = fg
        .symbols
        .iter()
        .find(|s| s.name == "ResolveReference" && s.parent.is_empty());
    assert!(
        leaked_has.is_none(),
        "HasAnyPendingItems must NOT surface as a top-level free function; got {leaked_has:?}"
    );
    assert!(
        leaked_resolve.is_none(),
        "ResolveReference must NOT surface as a top-level free function; got {leaked_resolve:?}"
    );
}

/// TC-A6 — `struct MYLIB_API MyStruct { … };`. The report did not isolate
/// a struct failure in the live data but flagged it as worth a test
/// because the same code path is involved. Minimal form: PASSES today
/// (struct extracts with correct kind).
#[test]
fn tc_a6_struct_with_api_macro_indexes_as_struct() {
    let src = "\
struct MYLIB_API MyStruct
{
    int Value;
    void Reset();
};
";
    let fg = parse_with_strip(src, &["MYLIB_API"], &[]);

    let s = find_symbol(&fg, "MyStruct").expect("MyStruct must index");
    assert_eq!(
        s.kind,
        SymbolKind::Struct,
        "MyStruct must classify as Struct (not Class)"
    );

    // Field `int Value;` is intentionally not emitted — the C++ parser
    // does not extract fields. Forward-declared method `Reset()`
    // likewise not emitted. Guard that NEITHER leaks as a top-level
    // free symbol.
    let leaked_field = fg
        .symbols
        .iter()
        .find(|s| s.name == "Value" && s.parent.is_empty());
    let leaked_method = fg
        .symbols
        .iter()
        .find(|s| s.name == "Reset" && s.parent.is_empty());
    assert!(
        leaked_field.is_none(),
        "field Value must not surface as top-level Symbol"
    );
    assert!(
        leaked_method.is_none(),
        "forward-decl Reset() must not surface as top-level Symbol"
    );
}

// =========================================================================
// Category B — inheritance graph gaps
// =========================================================================

/// TC-B1 — template-instantiation base classes. The original report
/// flagged that `class Foo : public TBaseTemplate<IBar>` produced no
/// Inherits edge. After the inheritance-query extension for
/// `template_type` nodes (commit pinning this test), the bare
/// template name DOES surface as the inheritance target — template
/// arguments are dropped to match the bare-name convention the
/// hierarchy walker keys on. Regression guard going forward.
#[test]
fn tc_b1_template_instantiation_base_emits_inherits_edge() {
    let src = "\
template <typename T>
class TBaseTemplate { public: void Send(); };

class IClientInterface { public: virtual void Get() = 0; };

class MyClient
    : public TBaseTemplate<IClientInterface>
{
public:
    void Send();
};
";
    let fg = parse_with_strip(src, &[], &[]);

    assert!(
        find_symbol(&fg, "MyClient").is_some(),
        "MyClient must index"
    );

    // The Inherits edge target may be `TBaseTemplate` (bare) or
    // `TBaseTemplate<IClientInterface>` (verbatim). Accept either by
    // checking the prefix.
    let has_template_base = fg.edges.iter().any(|e| {
        e.kind == EdgeKind::Inherits
            && e.from == "MyClient"
            && (e.to == "TBaseTemplate" || e.to.starts_with("TBaseTemplate<"))
    });
    assert!(
        has_template_base,
        "MyClient -> TBaseTemplate inherits edge must exist; got edges: {:?}",
        fg.edges
    );
}

/// TC-B3 — class-name collisions across files. The original report
/// flagged hierarchy-walker behaviour flattening third-party-library
/// types under a same-named engine type's node. That's a hierarchy
/// concern; at the PARSER layer, the only thing we can guard is that
/// two same-named classes in different files keep distinct identity
/// (different `file` fields) — the `(Language, name)`-keyed
/// `SymbolIndex` in `crates/code-graph-lang/src/lib.rs` then separates
/// them per-language. Same-language same-name in different files
/// remains a hierarchy-walker concern documented in CLAUDE.md.
#[test]
fn tc_b3_same_name_classes_in_different_files_both_index() {
    let p = CppParser::new().unwrap();
    let fg_a = p
        .parse_file(
            Path::new("/lib_a/object.h"),
            b"class SharedName { public: void Tick(); };",
        )
        .unwrap();
    let fg_b = p
        .parse_file(
            Path::new("/lib_b/object.h"),
            b"class SharedName { public: void clone(); };",
        )
        .unwrap();

    let a_sym = find_symbol(&fg_a, "SharedName").expect("lib_a SharedName must index");
    let b_sym = find_symbol(&fg_b, "SharedName").expect("lib_b SharedName must index");
    assert_ne!(
        a_sym.file, b_sym.file,
        "two same-named classes in different files must keep distinct `file` fields"
    );
}

// =========================================================================
// Category C — call graph resolution
// =========================================================================

/// TC-C3 — macro-defined function: when a `#define` produces a function
/// body via token-pasting (`##`), the original report observed the
/// macro identifier itself surfacing as a top-level "function" Symbol
/// in addition to (or instead of) the real generated definitions.
///
/// Minimal form: PASSES today (macro identifier does not leak as a
/// Symbol). Pinned as regression guard.
#[test]
fn tc_c3_macro_invoked_at_top_level_does_not_leak_macro_name() {
    let src = "\
#define DECLARE_RELEASE_FN(TypeName) \\
    void TypeName##_Release(int* P) { (void)P; }

DECLARE_RELEASE_FN(MyTypeA)
DECLARE_RELEASE_FN(MyTypeB)
";
    let fg = parse_with_strip(src, &[], &[]);

    let leaked = fg
        .symbols
        .iter()
        .find(|s| s.name == "DECLARE_RELEASE_FN" && s.parent.is_empty());
    assert!(
        leaked.is_none(),
        "DECLARE_RELEASE_FN must NOT surface as a top-level function Symbol; got {leaked:?}"
    );
}
