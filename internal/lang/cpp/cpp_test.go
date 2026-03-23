package cpp

import (
	"testing"

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
