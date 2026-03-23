package graph

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/danweinerdev/code-graph-mcp/internal/parser"
)

func TestSaveAndLoad(t *testing.T) {
	dir := t.TempDir()

	g := New()
	g.MergeFileGraph(makeFileGraph("/a.cpp", []parser.Symbol{
		sym("foo", parser.KindFunction, "/a.cpp"),
		sym("bar", parser.KindFunction, "/a.cpp"),
	}, []parser.Edge{
		callEdge("/a.cpp:foo", "/a.cpp:bar", "/a.cpp"),
		includeEdge("/a.cpp", "/b.h", "/a.cpp"),
	}))

	if err := g.Save(dir); err != nil {
		t.Fatalf("save failed: %v", err)
	}

	// Verify cache file exists.
	cachePath := filepath.Join(dir, cacheFileName)
	if _, err := os.Stat(cachePath); err != nil {
		t.Fatalf("cache file not found: %v", err)
	}

	// Load into a new graph.
	g2 := New()
	loaded, err := g2.Load(dir)
	if err != nil {
		t.Fatalf("load failed: %v", err)
	}
	if !loaded {
		t.Fatal("expected loaded=true")
	}

	// Verify contents.
	nodes, edges, files := g2.Stats()
	if nodes != 2 {
		t.Errorf("expected 2 nodes, got %d", nodes)
	}
	if edges != 2 {
		t.Errorf("expected 2 edges, got %d", edges)
	}
	if files != 1 {
		t.Errorf("expected 1 file, got %d", files)
	}

	// Verify symbol data survived round-trip.
	s := g2.SymbolDetail("/a.cpp:foo")
	if s == nil {
		t.Fatal("expected symbol /a.cpp:foo after load")
	}
	if s.Name != "foo" || s.Kind != parser.KindFunction {
		t.Errorf("unexpected symbol data: %+v", s)
	}
}

func TestLoadNonExistent(t *testing.T) {
	g := New()
	loaded, err := g.Load("/nonexistent/dir")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if loaded {
		t.Fatal("expected loaded=false for nonexistent dir")
	}
}

func TestIncomingCoupling(t *testing.T) {
	g := New()
	// a.cpp calls b.cpp:bar, c.cpp calls b.cpp:bar
	g.MergeFileGraph(makeFileGraph("/a.cpp", []parser.Symbol{
		sym("foo", parser.KindFunction, "/a.cpp"),
	}, []parser.Edge{
		callEdge("/a.cpp:foo", "/b.cpp:bar", "/a.cpp"),
	}))
	g.MergeFileGraph(makeFileGraph("/b.cpp", []parser.Symbol{
		sym("bar", parser.KindFunction, "/b.cpp"),
	}, nil))
	g.MergeFileGraph(makeFileGraph("/c.cpp", []parser.Symbol{
		sym("baz", parser.KindFunction, "/c.cpp"),
	}, []parser.Edge{
		callEdge("/c.cpp:baz", "/b.cpp:bar", "/c.cpp"),
		includeEdge("/c.cpp", "/b.cpp", "/c.cpp"),
	}))

	incoming := g.IncomingCoupling("/b.cpp")
	if incoming["/a.cpp"] != 1 {
		t.Errorf("expected /a.cpp incoming 1, got %d", incoming["/a.cpp"])
	}
	if incoming["/c.cpp"] != 2 { // 1 call + 1 include
		t.Errorf("expected /c.cpp incoming 2, got %d", incoming["/c.cpp"])
	}
}

func TestMermaidInheritance(t *testing.T) {
	g := New()
	g.MergeFileGraph(makeFileGraph("/a.cpp", []parser.Symbol{
		sym("Base", parser.KindClass, "/a.cpp"),
		sym("Middle", parser.KindClass, "/a.cpp"),
		sym("Derived", parser.KindClass, "/a.cpp"),
	}, []parser.Edge{
		inheritEdge("Middle", "Base", "/a.cpp"),
		inheritEdge("Derived", "Middle", "/a.cpp"),
	}))

	diagram := g.MermaidInheritance("Middle", 2, 30)
	if diagram == "" {
		t.Fatal("expected non-empty inheritance diagram")
	}
	t.Log(diagram)

	if !strings.Contains(diagram, "graph BT") {
		t.Error("expected bottom-top graph")
	}
	if !strings.Contains(diagram, "inherits") {
		t.Error("expected 'inherits' edge labels")
	}
	if !strings.Contains(diagram, "Base") {
		t.Error("expected Base in diagram")
	}
	if !strings.Contains(diagram, "Derived") {
		t.Error("expected Derived in diagram")
	}
}

func TestMermaidInheritanceUnknown(t *testing.T) {
	g := New()
	diagram := g.MermaidInheritance("Unknown", 1, 30)
	if diagram != "" {
		t.Error("expected empty diagram for unknown class")
	}
}

func TestAllFilePaths(t *testing.T) {
	g := New()
	g.MergeFileGraph(makeFileGraph("/a.cpp", nil, nil))
	g.MergeFileGraph(makeFileGraph("/b.cpp", nil, nil))

	paths := g.AllFilePaths()
	if len(paths) != 2 {
		t.Errorf("expected 2 paths, got %d", len(paths))
	}
}

func TestAllSymbols(t *testing.T) {
	g := New()
	g.MergeFileGraph(makeFileGraph("/a.cpp", []parser.Symbol{
		sym("foo", parser.KindFunction, "/a.cpp"),
		sym("bar", parser.KindFunction, "/a.cpp"),
	}, nil))

	symbols := g.AllSymbols()
	if len(symbols) != 2 {
		t.Errorf("expected 2 symbols, got %d", len(symbols))
	}
}
