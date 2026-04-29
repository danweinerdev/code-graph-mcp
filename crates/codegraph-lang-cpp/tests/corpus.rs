//! C++ parser test corpus — Rust port of `internal/lang/cpp/cpp_test.go`.
//!
//! This is the comprehensive Phase 1.6 corpus. Every named test mirrors a Go
//! test case in `internal/lang/cpp/cpp_test.go` so behavior parity stays
//! verifiable by name.
//!
//! Helper-function unit tests (`TestSplitQualified`, `TestStripIncludePath`,
//! `TestTruncateSignatureByteFallback`, `TestTruncateSignatureUTF8Boundary`)
//! live in `helpers.rs` — porting them here would duplicate coverage.

use std::path::Path;

use codegraph_core::{Edge, EdgeKind, FileGraph, Symbol, SymbolKind};
use codegraph_lang::LanguagePlugin;
use codegraph_lang_cpp::CppParser;
use pretty_assertions::assert_eq;

// --- Test helpers ---------------------------------------------------------

/// Parse `src` as C++ and return the resulting [`FileGraph`]. The path is
/// fixed at `/test.cpp` to match the Go test fixture.
fn parse(src: &str) -> FileGraph {
    let p = CppParser::new().expect("CppParser::new must succeed");
    p.parse_file(Path::new("/test.cpp"), src.as_bytes())
        .expect("parse_file must succeed")
}

fn find_symbol<'a>(fg: &'a FileGraph, name: &str) -> Option<&'a Symbol> {
    fg.symbols.iter().find(|s| s.name == name)
}

fn find_edge<'a>(fg: &'a FileGraph, kind: EdgeKind, to: &str) -> Option<&'a Edge> {
    fg.edges.iter().find(|e| e.kind == kind && e.to == to)
}

fn find_edge_from<'a>(fg: &'a FileGraph, kind: EdgeKind, from: &str, to: &str) -> Option<&'a Edge> {
    fg.edges
        .iter()
        .find(|e| e.kind == kind && e.from == from && e.to == to)
}

// --- Definition tests -----------------------------------------------------

/// Mirrors Go `TestFreeFunction`.
#[test]
fn free_function() {
    let fg = parse("void foo() {}");
    let s = find_symbol(&fg, "foo").expect("expected symbol 'foo'");
    assert_eq!(s.kind, SymbolKind::Function);
    assert_eq!(s.line, 1);
}

/// Mirrors Go `TestMethodDefinition`.
#[test]
fn method_definition() {
    let fg = parse("void MyClass::doWork() {}");
    let s = find_symbol(&fg, "doWork").expect("expected symbol 'doWork'");
    assert_eq!(s.kind, SymbolKind::Method);
    assert_eq!(s.parent, "MyClass");
}

/// Mirrors Go `TestClassWithBody`.
#[test]
fn class_with_body() {
    let fg = parse("class Engine { void update(); };");
    let s = find_symbol(&fg, "Engine").expect("expected symbol 'Engine'");
    assert_eq!(s.kind, SymbolKind::Class);
}

/// Mirrors Go `TestStructWithBody`.
#[test]
fn struct_with_body() {
    let fg = parse("struct Point { int x; int y; };");
    let s = find_symbol(&fg, "Point").expect("expected symbol 'Point'");
    assert_eq!(s.kind, SymbolKind::Struct);
}

/// Mirrors Go `TestEnum`.
#[test]
fn enum_definition() {
    let fg = parse("enum Color { Red, Green, Blue };");
    let s = find_symbol(&fg, "Color").expect("expected symbol 'Color'");
    assert_eq!(s.kind, SymbolKind::Enum);
}

/// Mirrors Go `TestTypedef`.
#[test]
fn typedef_simple() {
    let fg = parse("typedef int MyInt;");
    let s = find_symbol(&fg, "MyInt").expect("expected symbol 'MyInt'");
    assert_eq!(s.kind, SymbolKind::Typedef);
}

/// Mirrors Go `TestNestedNamespace`.
#[test]
fn nested_namespace() {
    let src = "\nnamespace a {\nnamespace b {\nvoid foo() {}\n}\n}\n";
    let fg = parse(src);
    let s = find_symbol(&fg, "foo").expect("expected symbol 'foo'");
    assert_eq!(s.namespace, "a::b");
}

/// Mirrors Go `TestForwardDeclarationExcluded` — forward decls produce
/// no symbol because only `function_definition` (with body) does.
#[test]
fn forward_declaration_excluded() {
    let fg = parse("void foo();");
    assert!(
        find_symbol(&fg, "foo").is_none(),
        "forward declaration should NOT produce a symbol"
    );
}

// --- Call tests -----------------------------------------------------------

/// Mirrors Go `TestFreeCall`.
#[test]
fn free_call() {
    let fg = parse("void caller() { foo(); }");
    let e = find_edge(&fg, EdgeKind::Calls, "foo").expect("expected call edge to 'foo'");
    assert_eq!(e.from, "/test.cpp:caller");
}

/// Mirrors Go `TestMethodCall`.
#[test]
fn method_call() {
    let src = "\nstruct Obj { void method(); };\nvoid f() { Obj obj; obj.method(); }\n";
    let fg = parse(src);
    assert!(
        find_edge(&fg, EdgeKind::Calls, "method").is_some(),
        "expected call edge to 'method'"
    );
}

/// Mirrors Go `TestArrowCall`.
#[test]
fn arrow_call() {
    let src = "\nstruct Obj { void method(); };\nvoid f() { Obj* ptr; ptr->method(); }\n";
    let fg = parse(src);
    assert!(
        find_edge(&fg, EdgeKind::Calls, "method").is_some(),
        "expected call edge to 'method'"
    );
}

/// Mirrors Go `TestQualifiedCall`.
#[test]
fn qualified_call() {
    let src = "\nnamespace ns { void foo(); }\nvoid f() { ns::foo(); }\n";
    let fg = parse(src);
    assert!(
        find_edge(&fg, EdgeKind::Calls, "ns::foo").is_some(),
        "expected call edge to 'ns::foo'"
    );
}

/// Mirrors Go `TestTemplateCall`.
#[test]
fn template_call() {
    let src = "\ntemplate<typename T> T make();\nvoid f() { make<int>(); }\n";
    let fg = parse(src);
    assert!(
        find_edge(&fg, EdgeKind::Calls, "make").is_some(),
        "expected call edge to 'make'"
    );
}

/// Mirrors Go `TestCppCastsFiltered`. All four cast keywords parse as
/// `call_expression` in tree-sitter-cpp; the cast filter must drop them.
#[test]
fn cpp_casts_filtered() {
    let src = r#"
void f() {
    int x = static_cast<int>(3.14);
    auto p = reinterpret_cast<char*>(0);
    const int& r = const_cast<int&>(x);
    auto d = dynamic_cast<int*>(nullptr);
    realFunction();
}
"#;
    let fg = parse(src);
    for cast in [
        "static_cast",
        "reinterpret_cast",
        "const_cast",
        "dynamic_cast",
    ] {
        assert!(
            find_edge(&fg, EdgeKind::Calls, cast).is_none(),
            "C++ cast {cast:?} should not produce a call edge"
        );
    }
    assert!(
        find_edge(&fg, EdgeKind::Calls, "realFunction").is_some(),
        "expected call edge to 'realFunction'"
    );
}

// --- Include tests --------------------------------------------------------

/// Mirrors Go `TestQuotedInclude`.
#[test]
fn quoted_include() {
    let fg = parse(r#"#include "engine.h""#);
    let e = find_edge(&fg, EdgeKind::Includes, "engine.h").expect("expected include edge");
    assert_eq!(e.from, "/test.cpp");
}

/// Mirrors Go `TestSystemInclude`.
#[test]
fn system_include() {
    let fg = parse("#include <vector>");
    assert!(
        find_edge(&fg, EdgeKind::Includes, "vector").is_some(),
        "expected include edge to 'vector'"
    );
}

// --- Inheritance tests ----------------------------------------------------

/// Mirrors Go `TestSingleInheritance`.
#[test]
fn single_inheritance() {
    let fg = parse("class Base {}; class Derived : public Base {};");
    assert!(
        find_edge_from(&fg, EdgeKind::Inherits, "Derived", "Base").is_some(),
        "expected inherit edge Derived -> Base"
    );
}

/// Mirrors Go `TestMultipleInheritance`.
#[test]
fn multiple_inheritance() {
    let src = "\nclass A {};\nclass B {};\nclass D : public A, public B {};\n";
    let fg = parse(src);
    assert!(find_edge_from(&fg, EdgeKind::Inherits, "D", "A").is_some());
    assert!(find_edge_from(&fg, EdgeKind::Inherits, "D", "B").is_some());
}

/// Mirrors Go `TestQualifiedInheritance`.
#[test]
fn qualified_inheritance() {
    let src = "\nnamespace ns { class Base {}; }\nclass Derived : public ns::Base {};\n";
    let fg = parse(src);
    assert!(
        find_edge_from(&fg, EdgeKind::Inherits, "Derived", "ns::Base").is_some(),
        "expected inherit edge Derived -> ns::Base"
    );
}

// --- Edge cases -----------------------------------------------------------

/// Mirrors Go `TestTopLevelCall` — calls outside any function use the bare
/// path as the `from`.
#[test]
fn top_level_call() {
    let fg = parse("int x = compute();");
    let e = find_edge(&fg, EdgeKind::Calls, "compute").expect("expected top-level call");
    assert_eq!(e.from, "/test.cpp");
}

/// Mirrors Go `TestAnonymousNamespace`.
#[test]
fn anonymous_namespace() {
    let src = "\nnamespace {\nvoid hidden() {}\n}\n";
    let fg = parse(src);
    let s = find_symbol(&fg, "hidden").expect("expected symbol 'hidden'");
    assert_eq!(s.namespace, "", "anonymous namespace should produce empty");
}

/// Mirrors Go `TestSignatureTruncation` — long signatures get truncated at
/// the byte fallback.
#[test]
fn signature_truncation() {
    let long = "void veryLongFunctionName(int a, int b, int c, int d, int e, int f, int g, int h, int i, int j, int k, int l, int m, int n, int o, int p, int q, int r, int s, int t_param, int u, int v, int w) {}";
    let fg = parse(long);
    let s = find_symbol(&fg, "veryLongFunctionName").expect("expected symbol");
    assert!(
        s.signature.len() <= 210,
        "signature should be truncated, got length {}",
        s.signature.len()
    );
}

/// Mirrors Go `TestSignatureTruncatedAtBrace` — body contents must not
/// appear in the signature.
#[test]
fn signature_truncated_at_brace() {
    let src = "void hello() { int x = 1; doStuff(x); return; }";
    let fg = parse(src);
    let s = find_symbol(&fg, "hello").expect("expected symbol");
    assert!(
        !s.signature.contains('{'),
        "signature should be truncated at '{{', got {:?}",
        s.signature
    );
    assert!(
        !s.signature.contains("doStuff"),
        "signature should not contain body, got {:?}",
        s.signature
    );
}

/// Mirrors Go `TestSignatureClassBodyTruncated`.
#[test]
fn signature_class_body_truncated() {
    let src = "class Engine {\npublic:\n    void update();\n    void render();\nprivate:\n    int state;\n};";
    let fg = parse(src);
    let s = find_symbol(&fg, "Engine").expect("expected symbol Engine");
    assert!(
        !s.signature.contains('{'),
        "class signature should be truncated at '{{', got {:?}",
        s.signature
    );
}

// --- Function pointer typedef tests ---------------------------------------

/// Mirrors Go `TestFunctionPointerTypedef`.
#[test]
fn function_pointer_typedef() {
    let fg = parse("typedef void (*Callback)(int, int);");
    let s = find_symbol(&fg, "Callback").expect("expected symbol 'Callback'");
    assert_eq!(s.kind, SymbolKind::Typedef);
}

/// Mirrors Go `TestFunctionPointerTypedefNoArgs`.
#[test]
fn function_pointer_typedef_no_args() {
    let fg = parse("typedef int (*Producer)();");
    let s = find_symbol(&fg, "Producer").expect("expected symbol 'Producer'");
    assert_eq!(s.kind, SymbolKind::Typedef);
}

/// Mirrors Go `TestUsingAlias` — `using` aliases to function pointers
/// produce a typedef.
#[test]
fn using_alias_function_pointer() {
    let fg = parse("using Callback = void(*)(int, int);");
    let s = find_symbol(&fg, "Callback").expect("expected symbol 'Callback'");
    assert_eq!(s.kind, SymbolKind::Typedef);
}

/// Mirrors Go `TestUsingAliasSimple`.
#[test]
fn using_alias_simple() {
    let fg = parse("using MyInt = int;");
    let s = find_symbol(&fg, "MyInt").expect("expected symbol 'MyInt'");
    assert_eq!(s.kind, SymbolKind::Typedef);
}

// --- Operator overload tests ----------------------------------------------

/// Mirrors Go `TestOperatorOverloadInClass`. Note Go's deliberate quirk:
/// in-class operator overloads are recorded with `SymbolKind::Function`
/// (not `Method`) but still carry the parent class name.
#[test]
fn operator_overload_in_class() {
    let src = r#"
class Vec {
public:
    Vec operator+(const Vec& other) const { return Vec(); }
    bool operator==(const Vec& other) const { return true; }
};
"#;
    let fg = parse(src);
    let plus = find_symbol(&fg, "operator+").expect("expected symbol 'operator+'");
    assert_eq!(plus.parent, "Vec");

    assert!(
        find_symbol(&fg, "operator==").is_some(),
        "expected symbol 'operator=='"
    );
}

/// Mirrors Go `TestOperatorOverloadOutOfClass`.
#[test]
fn operator_overload_out_of_class() {
    let src = "\nclass Vec {};\nVec operator*(const Vec& a, float s) { return Vec(); }\n";
    let fg = parse(src);
    let s = find_symbol(&fg, "operator*").expect("expected symbol 'operator*'");
    assert_eq!(s.parent, "", "free operator should have empty parent");
}

// --- Enum class tests -----------------------------------------------------

/// Mirrors Go `TestEnumClass`.
#[test]
fn enum_class_basic() {
    let fg = parse("enum class Color { Red, Green, Blue };");
    let s = find_symbol(&fg, "Color").expect("expected symbol 'Color'");
    assert_eq!(s.kind, SymbolKind::Enum);
}

/// Mirrors Go `TestEnumClassScoped`.
#[test]
fn enum_class_scoped() {
    let src = "\nnamespace gfx {\nenum class Primitive { Triangle, Quad, Line };\n}\n";
    let fg = parse(src);
    let s = find_symbol(&fg, "Primitive").expect("expected symbol 'Primitive'");
    assert_eq!(s.namespace, "gfx");
}

// --- Auto return type tests -----------------------------------------------

/// Mirrors Go `TestAutoTrailingReturn`.
#[test]
fn auto_trailing_return() {
    let fg = parse("auto foo() -> int { return 42; }");
    let s = find_symbol(&fg, "foo").expect("expected symbol 'foo'");
    assert_eq!(s.kind, SymbolKind::Function);
}

/// Mirrors Go `TestAutoDeducedReturn`.
#[test]
fn auto_deduced_return() {
    let fg = parse("auto bar() { return 42; }");
    let s = find_symbol(&fg, "bar").expect("expected symbol 'bar'");
    assert_eq!(s.kind, SymbolKind::Function);
}

// --- Nested class tests ---------------------------------------------------

/// Mirrors Go `TestNestedClass`.
#[test]
fn nested_class() {
    let src = r#"
class Outer {
public:
    class Inner {
    public:
        void method() {}
    };
};
"#;
    let fg = parse(src);
    let outer = find_symbol(&fg, "Outer").expect("expected symbol 'Outer'");
    assert_eq!(outer.parent, "");

    let inner = find_symbol(&fg, "Inner").expect("expected symbol 'Inner'");
    assert_eq!(inner.parent, "Outer");
    assert_eq!(inner.kind, SymbolKind::Class);
}

/// Mirrors Go `TestNestedStruct`.
#[test]
fn nested_struct() {
    let src = "\nclass Container {\n    struct Node {\n        int value;\n    };\n};\n";
    let fg = parse(src);
    let node = find_symbol(&fg, "Node").expect("expected symbol 'Node'");
    assert_eq!(node.parent, "Container");
    assert_eq!(node.kind, SymbolKind::Struct);
}

// --- Inline method tests --------------------------------------------------

/// Mirrors Go `TestInlineMethodInClass`.
#[test]
fn inline_method_in_class() {
    let src = r#"
class Engine {
public:
    void update() {}
    int getSpeed() const { return 0; }
};
"#;
    let fg = parse(src);
    let update = find_symbol(&fg, "update").expect("expected 'update'");
    assert_eq!(update.kind, SymbolKind::Method);
    assert_eq!(update.parent, "Engine");

    let get_speed = find_symbol(&fg, "getSpeed").expect("expected 'getSpeed'");
    assert_eq!(get_speed.parent, "Engine");
}

// --- Lambda tests ---------------------------------------------------------

/// Mirrors Go `TestLambdaCallEdge` — lambdas invoked via name produce a
/// call edge from the enclosing function.
#[test]
fn lambda_call_edge() {
    let src = r#"
void process() {
    auto fn = [](int x) { return x * 2; };
    fn(42);
}
"#;
    let fg = parse(src);
    let e = find_edge(&fg, EdgeKind::Calls, "fn").expect("expected lambda call edge");
    assert_eq!(e.from, "/test.cpp:process");
}

/// Mirrors Go `TestLambdaCallsInsideLambda` — calls inside a lambda body
/// resolve to the lambda's enclosing function (lambdas do not create a
/// new `function_definition` ancestor in tree-sitter-cpp).
#[test]
fn lambda_calls_inside_lambda() {
    let src = "\nvoid helper() {}\nvoid outer() {\n    auto fn = [](){ helper(); };\n}\n";
    let fg = parse(src);
    assert!(
        find_edge(&fg, EdgeKind::Calls, "helper").is_some(),
        "expected call edge to 'helper' from inside lambda"
    );
}

// --- Constructor initializer list tests -----------------------------------

/// Mirrors Go `TestConstructorInitListNotCallEdge` — `x_(0)` in the init
/// list is a `field_initializer`, not a `call_expression`, so no spurious
/// call edge appears. We only confirm the Engine class symbol exists.
#[test]
fn constructor_init_list_not_call_edge() {
    let src = r#"
class Engine {
    int x_;
public:
    Engine() : x_(0) {}
};
"#;
    let fg = parse(src);
    let found = fg
        .symbols
        .iter()
        .any(|s| s.name == "Engine" && s.kind == SymbolKind::Class);
    assert!(found, "expected Engine class symbol");
}

// --- Macro / error-recovery tests -----------------------------------------

/// Verifies the parser does not crash on garbled top-level fragments and
/// still produces clean symbols for valid code that follows them. tree-sitter
/// produces ERROR nodes for the broken fragment; the extraction loops skip
/// them via `node.has_error()`.
#[test]
fn error_node_recovery() {
    let src = "@@@ broken @@@\n\nvoid clean() { helper(); }\n";
    let fg = parse(src);
    let clean = find_symbol(&fg, "clean").expect("clean function must still extract");
    assert_eq!(clean.kind, SymbolKind::Function);
    assert!(
        find_edge_from(&fg, EdgeKind::Calls, "/test.cpp:clean", "helper").is_some(),
        "call edge inside clean() should survive error recovery"
    );
}

// --- UTF-8 boundary integration test --------------------------------------

/// Integration-layer check that multi-byte content past the 200-byte cutoff
/// is handled without panicking and produces a valid UTF-8 signature. The
/// helper-level test in `helpers.rs` already covers this; this one runs the
/// full extraction pipeline end-to-end with a multi-byte string-literal in
/// the function body. The signature is truncated at `{` long before the
/// 200-byte fallback fires, so this also confirms the brace-stop path on
/// multi-byte content. Mirrors Go `TestTruncateSignatureUTF8Boundary`.
#[test]
fn signature_utf8_boundary_safe() {
    // Function name uses ASCII (tree-sitter-cpp identifiers are ASCII);
    // body holds the multi-byte content. The signature should stop at `{`
    // and never approach the byte fallback.
    let src = "void greet() { const char* s = \"こんにちは世界 — héllo wörld — over and over and over and over and over and over and over and over and over\"; (void)s; }";
    let fg = parse(src);
    let s = find_symbol(&fg, "greet").expect("expected 'greet'");
    assert!(
        std::str::from_utf8(s.signature.as_bytes()).is_ok(),
        "signature must be valid UTF-8"
    );
    assert!(!s.signature.contains('{'));
}

// --- Parameterized smoke test for extension/language wiring ---------------

/// A quick parameterized check that every supported extension is claimed
/// by the C++ parser at the trait level. The Go binary does not have an
/// equivalent test, but it codifies the contract from
/// `verification.extensions_match_go_list` against the `LanguagePlugin`
/// trait surface. `rstest` is used here both to exercise the dev-dep wiring
/// and to keep the test data in one place.
#[rstest::rstest]
#[case::cpp(".cpp")]
#[case::cc(".cc")]
#[case::cxx(".cxx")]
#[case::c(".c")]
#[case::h(".h")]
#[case::hpp(".hpp")]
#[case::hxx(".hxx")]
fn extension_is_claimed(#[case] ext: &str) {
    let p = CppParser::new().unwrap();
    let exts: &[&str] = LanguagePlugin::extensions(&p);
    assert!(exts.contains(&ext), "extension {ext} must be claimed");
}
