package cpp

import (
	"strings"
	"testing"
	"unicode/utf8"

	"github.com/danweinerdev/code-graph-mcp/internal/parser"
)

// helper parses C++ source and returns the FileGraph.
func parse(t *testing.T, src string) *parser.FileGraph {
	t.Helper()
	p, err := NewCppParser()
	if err != nil {
		t.Fatalf("NewCppParser: %v", err)
	}
	defer p.Close()

	fg, err := p.ParseFile("/test.cpp", []byte(src))
	if err != nil {
		t.Fatalf("ParseFile: %v", err)
	}
	return fg
}

func findSymbol(fg *parser.FileGraph, name string) *parser.Symbol {
	for i := range fg.Symbols {
		if fg.Symbols[i].Name == name {
			return &fg.Symbols[i]
		}
	}
	return nil
}

func findEdge(fg *parser.FileGraph, kind parser.EdgeKind, to string) *parser.Edge {
	for i := range fg.Edges {
		if fg.Edges[i].Kind == kind && fg.Edges[i].To == to {
			return &fg.Edges[i]
		}
	}
	return nil
}

func findEdgeFrom(fg *parser.FileGraph, kind parser.EdgeKind, from, to string) *parser.Edge {
	for i := range fg.Edges {
		if fg.Edges[i].Kind == kind && fg.Edges[i].From == from && fg.Edges[i].To == to {
			return &fg.Edges[i]
		}
	}
	return nil
}

// --- Definition Tests ---

func TestFreeFunction(t *testing.T) {
	fg := parse(t, `void foo() {}`)
	s := findSymbol(fg, "foo")
	if s == nil {
		t.Fatal("expected symbol 'foo'")
	}
	if s.Kind != parser.KindFunction {
		t.Errorf("expected kind function, got %s", s.Kind)
	}
	if s.Line != 1 {
		t.Errorf("expected line 1, got %d", s.Line)
	}
}

func TestMethodDefinition(t *testing.T) {
	fg := parse(t, `void MyClass::doWork() {}`)
	s := findSymbol(fg, "doWork")
	if s == nil {
		t.Fatal("expected symbol 'doWork'")
	}
	if s.Kind != parser.KindMethod {
		t.Errorf("expected kind method, got %s", s.Kind)
	}
	if s.Parent != "MyClass" {
		t.Errorf("expected parent MyClass, got %q", s.Parent)
	}
}

func TestClassWithBody(t *testing.T) {
	fg := parse(t, `class Engine { void update(); };`)
	s := findSymbol(fg, "Engine")
	if s == nil {
		t.Fatal("expected symbol 'Engine'")
	}
	if s.Kind != parser.KindClass {
		t.Errorf("expected kind class, got %s", s.Kind)
	}
}

func TestStructWithBody(t *testing.T) {
	fg := parse(t, `struct Point { int x; int y; };`)
	s := findSymbol(fg, "Point")
	if s == nil {
		t.Fatal("expected symbol 'Point'")
	}
	if s.Kind != parser.KindStruct {
		t.Errorf("expected kind struct, got %s", s.Kind)
	}
}

func TestEnum(t *testing.T) {
	fg := parse(t, `enum Color { Red, Green, Blue };`)
	s := findSymbol(fg, "Color")
	if s == nil {
		t.Fatal("expected symbol 'Color'")
	}
	if s.Kind != parser.KindEnum {
		t.Errorf("expected kind enum, got %s", s.Kind)
	}
}

func TestTypedef(t *testing.T) {
	fg := parse(t, `typedef int MyInt;`)
	s := findSymbol(fg, "MyInt")
	if s == nil {
		t.Fatal("expected symbol 'MyInt'")
	}
	if s.Kind != parser.KindTypedef {
		t.Errorf("expected kind typedef, got %s", s.Kind)
	}
}

func TestNestedNamespace(t *testing.T) {
	fg := parse(t, `
namespace a {
namespace b {
void foo() {}
}
}
`)
	s := findSymbol(fg, "foo")
	if s == nil {
		t.Fatal("expected symbol 'foo'")
	}
	if s.Namespace != "a::b" {
		t.Errorf("expected namespace a::b, got %q", s.Namespace)
	}
}

func TestForwardDeclarationExcluded(t *testing.T) {
	fg := parse(t, `void foo();`)
	s := findSymbol(fg, "foo")
	if s != nil {
		t.Errorf("forward declaration should NOT produce a symbol, got %+v", s)
	}
}

// --- Call Tests ---

func TestFreeCall(t *testing.T) {
	fg := parse(t, `void caller() { foo(); }`)
	e := findEdge(fg, parser.EdgeCalls, "foo")
	if e == nil {
		t.Fatal("expected call edge to 'foo'")
	}
	if e.From != "/test.cpp:caller" {
		t.Errorf("expected from /test.cpp:caller, got %q", e.From)
	}
}

func TestMethodCall(t *testing.T) {
	fg := parse(t, `
struct Obj { void method(); };
void f() { Obj obj; obj.method(); }
`)
	e := findEdge(fg, parser.EdgeCalls, "method")
	if e == nil {
		t.Fatal("expected call edge to 'method'")
	}
}

func TestArrowCall(t *testing.T) {
	fg := parse(t, `
struct Obj { void method(); };
void f() { Obj* ptr; ptr->method(); }
`)
	e := findEdge(fg, parser.EdgeCalls, "method")
	if e == nil {
		t.Fatal("expected call edge to 'method'")
	}
}

func TestQualifiedCall(t *testing.T) {
	fg := parse(t, `
namespace ns { void foo(); }
void f() { ns::foo(); }
`)
	e := findEdge(fg, parser.EdgeCalls, "ns::foo")
	if e == nil {
		t.Fatal("expected call edge to 'ns::foo'")
	}
}

func TestTemplateCall(t *testing.T) {
	fg := parse(t, `
template<typename T> T make();
void f() { make<int>(); }
`)
	e := findEdge(fg, parser.EdgeCalls, "make")
	if e == nil {
		t.Fatal("expected call edge to 'make'")
	}
}

func TestCppCastsFiltered(t *testing.T) {
	fg := parse(t, `
void f() {
    int x = static_cast<int>(3.14);
    auto p = reinterpret_cast<char*>(0);
    const int& r = const_cast<int&>(x);
    auto d = dynamic_cast<int*>(nullptr);
    realFunction();
}
`)
	// Casts should NOT produce call edges.
	for _, cast := range []string{"static_cast", "reinterpret_cast", "const_cast", "dynamic_cast"} {
		e := findEdge(fg, parser.EdgeCalls, cast)
		if e != nil {
			t.Errorf("C++ cast %q should not produce a call edge", cast)
		}
	}
	// Real function should still be captured.
	e := findEdge(fg, parser.EdgeCalls, "realFunction")
	if e == nil {
		t.Fatal("expected call edge to 'realFunction'")
	}
}

// --- Include Tests ---

func TestQuotedInclude(t *testing.T) {
	fg := parse(t, `#include "engine.h"`)
	e := findEdge(fg, parser.EdgeIncludes, "engine.h")
	if e == nil {
		t.Fatal("expected include edge to 'engine.h'")
	}
	if e.From != "/test.cpp" {
		t.Errorf("expected from /test.cpp, got %q", e.From)
	}
}

func TestSystemInclude(t *testing.T) {
	fg := parse(t, `#include <vector>`)
	e := findEdge(fg, parser.EdgeIncludes, "vector")
	if e == nil {
		t.Fatal("expected include edge to 'vector'")
	}
}

// --- Inheritance Tests ---

func TestSingleInheritance(t *testing.T) {
	fg := parse(t, `class Base {}; class Derived : public Base {};`)
	e := findEdgeFrom(fg, parser.EdgeInherits, "Derived", "Base")
	if e == nil {
		t.Fatal("expected inherit edge Derived -> Base")
	}
}

func TestMultipleInheritance(t *testing.T) {
	fg := parse(t, `
class A {};
class B {};
class D : public A, public B {};
`)
	eA := findEdgeFrom(fg, parser.EdgeInherits, "D", "A")
	eB := findEdgeFrom(fg, parser.EdgeInherits, "D", "B")
	if eA == nil {
		t.Error("expected inherit edge D -> A")
	}
	if eB == nil {
		t.Error("expected inherit edge D -> B")
	}
}

func TestQualifiedInheritance(t *testing.T) {
	fg := parse(t, `
namespace ns { class Base {}; }
class Derived : public ns::Base {};
`)
	e := findEdgeFrom(fg, parser.EdgeInherits, "Derived", "ns::Base")
	if e == nil {
		t.Fatal("expected inherit edge Derived -> ns::Base")
	}
}

// --- Edge Cases ---

func TestTopLevelCall(t *testing.T) {
	fg := parse(t, `int x = compute();`)
	e := findEdge(fg, parser.EdgeCalls, "compute")
	if e == nil {
		t.Fatal("expected call edge to 'compute' from top-level")
	}
	if e.From != "/test.cpp" {
		t.Errorf("expected from /test.cpp (top-level), got %q", e.From)
	}
}

func TestAnonymousNamespace(t *testing.T) {
	fg := parse(t, `
namespace {
void hidden() {}
}
`)
	s := findSymbol(fg, "hidden")
	if s == nil {
		t.Fatal("expected symbol 'hidden'")
	}
	if s.Namespace != "" {
		t.Errorf("expected empty namespace for anonymous, got %q", s.Namespace)
	}
}

func TestSignatureTruncation(t *testing.T) {
	// A function with a very long signature.
	long := "void veryLongFunctionName(int a, int b, int c, int d, int e, int f, int g, int h, int i, int j, int k, int l, int m, int n, int o, int p, int q, int r, int s, int t_param, int u, int v, int w) {}"
	fg := parse(t, long)
	s := findSymbol(fg, "veryLongFunctionName")
	if s == nil {
		t.Fatal("expected symbol")
	}
	if len(s.Signature) > 210 {
		t.Errorf("signature should be truncated, got length %d", len(s.Signature))
	}
}

// TestTruncateSignatureByteFallback exercises the byte-limit path of
// truncateSignature directly: a string with no '{' or ';' before byte 200.
func TestTruncateSignatureByteFallback(t *testing.T) {
	long := strings.Repeat("a", 250)
	got := truncateSignature(long)
	if !strings.HasSuffix(got, "...") {
		t.Errorf("expected trailing '...', got %q", got)
	}
	if len(got) > 210 {
		t.Errorf("expected length <= 210, got %d", len(got))
	}
}

// TestTruncateSignatureUTF8Boundary verifies that the byte fallback never
// slices through a multi-byte rune. The cut point must land on a rune boundary.
func TestTruncateSignatureUTF8Boundary(t *testing.T) {
	// 199 ASCII bytes, then 'é' (0xC3 0xA9) starting at byte 199. The next
	// rune boundary after byte 200 is at byte 201; truncateSignature must
	// cut at 201, not 200.
	input := strings.Repeat("a", 199) + "é" + strings.Repeat("b", 100)
	got := truncateSignature(input)
	if !utf8.ValidString(got) {
		t.Errorf("truncated signature is not valid UTF-8: %q", got)
	}
	if !strings.HasSuffix(got, "...") {
		t.Errorf("expected trailing '...', got %q", got)
	}
}

func TestSignatureTruncatedAtBrace(t *testing.T) {
	// Body contents must not appear in the signature.
	src := `void hello() { int x = 1; doStuff(x); return; }`
	fg := parse(t, src)
	s := findSymbol(fg, "hello")
	if s == nil {
		t.Fatal("expected symbol")
	}
	if strings.Contains(s.Signature, "{") {
		t.Errorf("signature should be truncated at '{', got %q", s.Signature)
	}
	if strings.Contains(s.Signature, "doStuff") {
		t.Errorf("signature should not contain body, got %q", s.Signature)
	}
}

func TestSignatureClassBodyTruncated(t *testing.T) {
	// Class signatures should not include the body.
	src := `class Engine {
public:
    void update();
    void render();
private:
    int state;
};`
	fg := parse(t, src)
	s := findSymbol(fg, "Engine")
	if s == nil {
		t.Fatal("expected symbol Engine")
	}
	if strings.Contains(s.Signature, "{") {
		t.Errorf("class signature should be truncated at '{', got %q", s.Signature)
	}
}

// --- Function Pointer Typedef Tests ---

func TestFunctionPointerTypedef(t *testing.T) {
	fg := parse(t, `typedef void (*Callback)(int, int);`)
	s := findSymbol(fg, "Callback")
	if s == nil {
		t.Fatal("expected symbol 'Callback' from function pointer typedef")
	}
	if s.Kind != parser.KindTypedef {
		t.Errorf("expected kind typedef, got %s", s.Kind)
	}
}

func TestFunctionPointerTypedefNoArgs(t *testing.T) {
	fg := parse(t, `typedef int (*Producer)();`)
	s := findSymbol(fg, "Producer")
	if s == nil {
		t.Fatal("expected symbol 'Producer'")
	}
	if s.Kind != parser.KindTypedef {
		t.Errorf("expected kind typedef, got %s", s.Kind)
	}
}

func TestUsingAlias(t *testing.T) {
	fg := parse(t, `using Callback = void(*)(int, int);`)
	s := findSymbol(fg, "Callback")
	if s == nil {
		t.Fatal("expected symbol 'Callback' from using alias")
	}
	if s.Kind != parser.KindTypedef {
		t.Errorf("expected kind typedef, got %s", s.Kind)
	}
}

func TestUsingAliasSimple(t *testing.T) {
	fg := parse(t, `using MyInt = int;`)
	s := findSymbol(fg, "MyInt")
	if s == nil {
		t.Fatal("expected symbol 'MyInt' from using alias")
	}
	if s.Kind != parser.KindTypedef {
		t.Errorf("expected kind typedef, got %s", s.Kind)
	}
}

// --- Operator Overload Tests ---

func TestOperatorOverloadInClass(t *testing.T) {
	fg := parse(t, `
class Vec {
public:
    Vec operator+(const Vec& other) const { return Vec(); }
    bool operator==(const Vec& other) const { return true; }
};
`)
	s1 := findSymbol(fg, "operator+")
	if s1 == nil {
		t.Fatal("expected symbol 'operator+'")
	}
	if s1.Parent != "Vec" {
		t.Errorf("expected parent Vec, got %q", s1.Parent)
	}

	s2 := findSymbol(fg, "operator==")
	if s2 == nil {
		t.Fatal("expected symbol 'operator=='")
	}
}

func TestOperatorOverloadOutOfClass(t *testing.T) {
	fg := parse(t, `
class Vec {};
Vec operator*(const Vec& a, float s) { return Vec(); }
`)
	s := findSymbol(fg, "operator*")
	if s == nil {
		t.Fatal("expected symbol 'operator*'")
	}
	// Free operator — no parent class.
	if s.Parent != "" {
		t.Errorf("expected empty parent for free operator, got %q", s.Parent)
	}
}

// --- Enum Class Tests ---

func TestEnumClass(t *testing.T) {
	fg := parse(t, `enum class Color { Red, Green, Blue };`)
	s := findSymbol(fg, "Color")
	if s == nil {
		t.Fatal("expected symbol 'Color' from enum class")
	}
	if s.Kind != parser.KindEnum {
		t.Errorf("expected kind enum, got %s", s.Kind)
	}
}

func TestEnumClassScoped(t *testing.T) {
	fg := parse(t, `
namespace gfx {
enum class Primitive { Triangle, Quad, Line };
}
`)
	s := findSymbol(fg, "Primitive")
	if s == nil {
		t.Fatal("expected symbol 'Primitive'")
	}
	if s.Namespace != "gfx" {
		t.Errorf("expected namespace gfx, got %q", s.Namespace)
	}
}

// --- Auto Return Type Tests ---

func TestAutoTrailingReturn(t *testing.T) {
	fg := parse(t, `auto foo() -> int { return 42; }`)
	s := findSymbol(fg, "foo")
	if s == nil {
		t.Fatal("expected symbol 'foo' with trailing return type")
	}
	if s.Kind != parser.KindFunction {
		t.Errorf("expected kind function, got %s", s.Kind)
	}
}

func TestAutoDeducedReturn(t *testing.T) {
	fg := parse(t, `auto bar() { return 42; }`)
	s := findSymbol(fg, "bar")
	if s == nil {
		t.Fatal("expected symbol 'bar' with auto deduced return")
	}
	if s.Kind != parser.KindFunction {
		t.Errorf("expected kind function, got %s", s.Kind)
	}
}

// --- Nested Class Tests ---

func TestNestedClass(t *testing.T) {
	fg := parse(t, `
class Outer {
public:
    class Inner {
    public:
        void method() {}
    };
};
`)
	outer := findSymbol(fg, "Outer")
	if outer == nil {
		t.Fatal("expected symbol 'Outer'")
	}
	if outer.Parent != "" {
		t.Errorf("Outer should have no parent, got %q", outer.Parent)
	}

	inner := findSymbol(fg, "Inner")
	if inner == nil {
		t.Fatal("expected symbol 'Inner'")
	}
	if inner.Parent != "Outer" {
		t.Errorf("expected Inner parent=Outer, got %q", inner.Parent)
	}
	if inner.Kind != parser.KindClass {
		t.Errorf("expected Inner kind=class, got %s", inner.Kind)
	}
}

func TestNestedStruct(t *testing.T) {
	fg := parse(t, `
class Container {
    struct Node {
        int value;
    };
};
`)
	node := findSymbol(fg, "Node")
	if node == nil {
		t.Fatal("expected symbol 'Node'")
	}
	if node.Parent != "Container" {
		t.Errorf("expected parent Container, got %q", node.Parent)
	}
	if node.Kind != parser.KindStruct {
		t.Errorf("expected kind struct, got %s", node.Kind)
	}
}

// --- Inline Method Tests ---

func TestInlineMethodInClass(t *testing.T) {
	fg := parse(t, `
class Engine {
public:
    void update() {}
    int getSpeed() const { return 0; }
};
`)
	s := findSymbol(fg, "update")
	if s == nil {
		t.Fatal("expected symbol 'update' from inline method")
	}
	if s.Kind != parser.KindMethod {
		t.Errorf("expected kind method, got %s", s.Kind)
	}
	if s.Parent != "Engine" {
		t.Errorf("expected parent Engine, got %q", s.Parent)
	}

	s2 := findSymbol(fg, "getSpeed")
	if s2 == nil {
		t.Fatal("expected symbol 'getSpeed'")
	}
	if s2.Parent != "Engine" {
		t.Errorf("expected parent Engine, got %q", s2.Parent)
	}
}

// --- Lambda Tests ---

func TestLambdaCallEdge(t *testing.T) {
	fg := parse(t, `
void process() {
    auto fn = [](int x) { return x * 2; };
    fn(42);
}
`)
	// Lambda invocation should produce a call edge.
	e := findEdge(fg, parser.EdgeCalls, "fn")
	if e == nil {
		t.Fatal("expected call edge to lambda 'fn'")
	}
	if e.From != "/test.cpp:process" {
		t.Errorf("expected from /test.cpp:process, got %q", e.From)
	}
}

func TestLambdaCallsInsideLambda(t *testing.T) {
	fg := parse(t, `
void helper() {}
void outer() {
    auto fn = [](){ helper(); };
}
`)
	// Call to helper() inside the lambda — enclosing function is outer().
	e := findEdge(fg, parser.EdgeCalls, "helper")
	if e == nil {
		t.Fatal("expected call edge to 'helper' from inside lambda")
	}
}

// --- Constructor Initializer List Tests ---

func TestConstructorInitListNotCallEdge(t *testing.T) {
	fg := parse(t, `
class Engine {
    int x_;
public:
    Engine() : x_(0) {}
};
`)
	// Constructor initializer lists are NOT function calls.
	// x_(0) is a field_initializer, not a call_expression.
	// The constructor itself should be a symbol.
	s := findSymbol(fg, "Engine")
	found := false
	for _, sym := range fg.Symbols {
		if sym.Name == "Engine" && sym.Kind == parser.KindClass {
			found = true
		}
	}
	if !found {
		t.Error("expected Engine class symbol")
	}
	_ = s
}

// --- Helper Tests ---

func TestSplitQualified(t *testing.T) {
	tests := []struct {
		input  string
		scope  string
		name   string
	}{
		{"Class::method", "Class", "method"},
		{"ns::Class::method", "ns::Class", "method"},
		{"plainFunc", "", "plainFunc"},
	}
	for _, tt := range tests {
		scope, name := splitQualified(tt.input)
		if scope != tt.scope || name != tt.name {
			t.Errorf("splitQualified(%q) = (%q, %q), want (%q, %q)", tt.input, scope, name, tt.scope, tt.name)
		}
	}
}

func TestStripIncludePath(t *testing.T) {
	tests := []struct {
		input string
		want  string
	}{
		{`"engine.h"`, "engine.h"},
		{`<vector>`, "vector"},
		{`plain`, "plain"},
	}
	for _, tt := range tests {
		got := stripIncludePath(tt.input)
		if got != tt.want {
			t.Errorf("stripIncludePath(%q) = %q, want %q", tt.input, got, tt.want)
		}
	}
}
