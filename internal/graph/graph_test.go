package graph

import (
	"sort"
	"strings"
	"sync"
	"testing"

	"github.com/danweinerdev/code-graph-mcp/internal/parser"
)

func makeFileGraph(path string, symbols []parser.Symbol, edges []parser.Edge) *parser.FileGraph {
	return &parser.FileGraph{Path: path, Symbols: symbols, Edges: edges}
}

func sym(name string, kind parser.SymbolKind, file string) parser.Symbol {
	return parser.Symbol{Name: name, Kind: kind, File: file, Line: 1}
}

func callEdge(from, to, file string) parser.Edge {
	return parser.Edge{From: from, To: to, Kind: parser.EdgeCalls, File: file, Line: 1}
}

func includeEdge(from, to, file string) parser.Edge {
	return parser.Edge{From: from, To: to, Kind: parser.EdgeIncludes, File: file, Line: 1}
}

func inheritEdge(from, to, file string) parser.Edge {
	return parser.Edge{From: from, To: to, Kind: parser.EdgeInherits, File: file, Line: 1}
}

// --- 4.2: MergeFileGraph and RemoveFile ---

func TestMergeFileGraph(t *testing.T) {
	g := New()
	fg := makeFileGraph("/a.cpp", []parser.Symbol{
		sym("foo", parser.KindFunction, "/a.cpp"),
	}, []parser.Edge{
		callEdge("/a.cpp:foo", "bar", "/a.cpp"),
	})

	g.MergeFileGraph(fg)

	if len(g.nodes) != 1 {
		t.Fatalf("expected 1 node, got %d", len(g.nodes))
	}
	if g.nodes["/a.cpp:foo"] == nil {
		t.Fatal("expected node /a.cpp:foo")
	}
}

func TestMergeFileGraphIdempotent(t *testing.T) {
	g := New()
	fg := makeFileGraph("/a.cpp", []parser.Symbol{
		sym("foo", parser.KindFunction, "/a.cpp"),
	}, nil)

	g.MergeFileGraph(fg)
	g.MergeFileGraph(fg) // re-merge same file

	if len(g.nodes) != 1 {
		t.Fatalf("expected 1 node after re-merge, got %d", len(g.nodes))
	}
}

func TestMergeTwoFiles(t *testing.T) {
	g := New()
	g.MergeFileGraph(makeFileGraph("/a.cpp", []parser.Symbol{
		sym("foo", parser.KindFunction, "/a.cpp"),
	}, nil))
	g.MergeFileGraph(makeFileGraph("/b.cpp", []parser.Symbol{
		sym("bar", parser.KindFunction, "/b.cpp"),
	}, nil))

	if len(g.nodes) != 2 {
		t.Fatalf("expected 2 nodes, got %d", len(g.nodes))
	}
}

func TestRemoveFile(t *testing.T) {
	g := New()
	g.MergeFileGraph(makeFileGraph("/a.cpp", []parser.Symbol{
		sym("foo", parser.KindFunction, "/a.cpp"),
	}, []parser.Edge{
		callEdge("/a.cpp:foo", "bar", "/a.cpp"),
	}))
	g.MergeFileGraph(makeFileGraph("/b.cpp", []parser.Symbol{
		sym("bar", parser.KindFunction, "/b.cpp"),
	}, nil))

	g.RemoveFile("/a.cpp")

	if len(g.nodes) != 1 {
		t.Fatalf("expected 1 node after remove, got %d", len(g.nodes))
	}
	if g.nodes["/a.cpp:foo"] != nil {
		t.Fatal("node /a.cpp:foo should be removed")
	}
	if g.nodes["/b.cpp:bar"] == nil {
		t.Fatal("node /b.cpp:bar should still exist")
	}
}

// --- 4.3: FileSymbols and SymbolDetail ---

func TestFileSymbols(t *testing.T) {
	g := New()
	g.MergeFileGraph(makeFileGraph("/a.cpp", []parser.Symbol{
		sym("foo", parser.KindFunction, "/a.cpp"),
		sym("bar", parser.KindFunction, "/a.cpp"),
	}, nil))

	syms := g.FileSymbols("/a.cpp")
	if len(syms) != 2 {
		t.Fatalf("expected 2 symbols, got %d", len(syms))
	}

	// Unknown file.
	syms = g.FileSymbols("/unknown.cpp")
	if len(syms) != 0 {
		t.Fatalf("expected 0 symbols for unknown file, got %d", len(syms))
	}
}

func TestSymbolDetail(t *testing.T) {
	g := New()
	g.MergeFileGraph(makeFileGraph("/a.cpp", []parser.Symbol{
		sym("foo", parser.KindFunction, "/a.cpp"),
	}, nil))

	s := g.SymbolDetail("/a.cpp:foo")
	if s == nil {
		t.Fatal("expected symbol detail")
	}
	if s.Name != "foo" {
		t.Errorf("expected name foo, got %s", s.Name)
	}

	// Unknown ID.
	s = g.SymbolDetail("/a.cpp:unknown")
	if s != nil {
		t.Fatal("expected nil for unknown symbol")
	}
}

// --- 4.4: SearchSymbols ---

func TestSearchSymbolsSubstring(t *testing.T) {
	g := New()
	g.MergeFileGraph(makeFileGraph("/a.cpp", []parser.Symbol{
		sym("fooBar", parser.KindFunction, "/a.cpp"),
		sym("bazQux", parser.KindFunction, "/a.cpp"),
	}, nil))

	results := g.SearchSymbols("foo", "")
	if len(results) != 1 || results[0].Name != "fooBar" {
		t.Fatalf("expected fooBar, got %v", results)
	}

	// Case insensitive.
	results = g.SearchSymbols("FOOBAR", "")
	if len(results) != 1 {
		t.Fatalf("expected case-insensitive match, got %d", len(results))
	}
}

func TestSearchSymbolsRegex(t *testing.T) {
	g := New()
	g.MergeFileGraph(makeFileGraph("/a.cpp", []parser.Symbol{
		sym("update", parser.KindMethod, "/a.cpp"),
		sym("render", parser.KindMethod, "/a.cpp"),
		sym("init", parser.KindFunction, "/a.cpp"),
	}, nil))

	results := g.SearchSymbols("^(update|render)$", "")
	if len(results) != 2 {
		t.Fatalf("expected 2 regex matches, got %d", len(results))
	}
}

func TestSearchSymbolsKindFilter(t *testing.T) {
	g := New()
	g.MergeFileGraph(makeFileGraph("/a.cpp", []parser.Symbol{
		sym("Engine", parser.KindClass, "/a.cpp"),
		sym("update", parser.KindFunction, "/a.cpp"),
	}, nil))

	results := g.SearchSymbols("", parser.KindClass)
	if len(results) != 1 || results[0].Name != "Engine" {
		t.Fatalf("expected Engine class, got %v", results)
	}
}

func TestSearchSymbolsNoMatch(t *testing.T) {
	g := New()
	g.MergeFileGraph(makeFileGraph("/a.cpp", []parser.Symbol{
		sym("foo", parser.KindFunction, "/a.cpp"),
	}, nil))

	results := g.SearchSymbols("zzzzz", "")
	if len(results) != 0 {
		t.Fatalf("expected no matches, got %d", len(results))
	}
}

// --- 4.5: Callers and Callees ---

func TestCallersLinearChain(t *testing.T) {
	g := New()
	// A calls B, B calls C
	g.MergeFileGraph(makeFileGraph("/a.cpp", []parser.Symbol{
		sym("A", parser.KindFunction, "/a.cpp"),
		sym("B", parser.KindFunction, "/a.cpp"),
		sym("C", parser.KindFunction, "/a.cpp"),
	}, []parser.Edge{
		callEdge("/a.cpp:A", "/a.cpp:B", "/a.cpp"),
		callEdge("/a.cpp:B", "/a.cpp:C", "/a.cpp"),
	}))

	// Direct callers of C.
	callers := g.Callers("/a.cpp:C", 1)
	if len(callers) != 1 || callers[0].SymbolID != "/a.cpp:B" {
		t.Fatalf("expected B as caller of C, got %v", callers)
	}

	// Transitive callers of C (depth 2).
	callers = g.Callers("/a.cpp:C", 2)
	if len(callers) != 2 {
		t.Fatalf("expected 2 transitive callers, got %d", len(callers))
	}
}

func TestCalleesLinearChain(t *testing.T) {
	g := New()
	g.MergeFileGraph(makeFileGraph("/a.cpp", []parser.Symbol{
		sym("A", parser.KindFunction, "/a.cpp"),
		sym("B", parser.KindFunction, "/a.cpp"),
		sym("C", parser.KindFunction, "/a.cpp"),
	}, []parser.Edge{
		callEdge("/a.cpp:A", "/a.cpp:B", "/a.cpp"),
		callEdge("/a.cpp:B", "/a.cpp:C", "/a.cpp"),
	}))

	callees := g.Callees("/a.cpp:A", 1)
	if len(callees) != 1 || callees[0].SymbolID != "/a.cpp:B" {
		t.Fatalf("expected B as callee of A, got %v", callees)
	}

	callees = g.Callees("/a.cpp:A", 2)
	if len(callees) != 2 {
		t.Fatalf("expected 2 transitive callees, got %d", len(callees))
	}
}

func TestCallersCycle(t *testing.T) {
	g := New()
	// A calls B, B calls A (cycle)
	g.MergeFileGraph(makeFileGraph("/a.cpp", []parser.Symbol{
		sym("A", parser.KindFunction, "/a.cpp"),
		sym("B", parser.KindFunction, "/a.cpp"),
	}, []parser.Edge{
		callEdge("/a.cpp:A", "/a.cpp:B", "/a.cpp"),
		callEdge("/a.cpp:B", "/a.cpp:A", "/a.cpp"),
	}))

	// Should not infinite loop.
	callers := g.Callers("/a.cpp:A", 10)
	if len(callers) != 1 {
		t.Fatalf("expected 1 caller (B) despite cycle, got %d", len(callers))
	}
}

func TestCallersUnknownSymbol(t *testing.T) {
	g := New()
	callers := g.Callers("/unknown:func", 1)
	if len(callers) != 0 {
		t.Fatalf("expected empty for unknown symbol, got %d", len(callers))
	}
}

func TestCallersDepthZero(t *testing.T) {
	g := New()
	g.MergeFileGraph(makeFileGraph("/a.cpp", []parser.Symbol{
		sym("A", parser.KindFunction, "/a.cpp"),
		sym("B", parser.KindFunction, "/a.cpp"),
	}, []parser.Edge{
		callEdge("/a.cpp:A", "/a.cpp:B", "/a.cpp"),
	}))

	// depth=0 treated as depth=1
	callers := g.Callers("/a.cpp:B", 0)
	if len(callers) != 1 {
		t.Fatalf("expected 1 caller with depth=0, got %d", len(callers))
	}
}

// --- 4.6: FileDependencies ---

func TestFileDependencies(t *testing.T) {
	g := New()
	g.MergeFileGraph(makeFileGraph("/a.cpp", nil, []parser.Edge{
		includeEdge("/a.cpp", "engine.h", "/a.cpp"),
		includeEdge("/a.cpp", "utils.h", "/a.cpp"),
	}))

	deps := g.FileDependencies("/a.cpp")
	if len(deps) != 2 {
		t.Fatalf("expected 2 deps, got %d", len(deps))
	}

	deps = g.FileDependencies("/unknown.cpp")
	if len(deps) != 0 {
		t.Fatalf("expected 0 deps for unknown, got %d", len(deps))
	}
}

// --- 4.7: DetectCycles ---

func TestDetectCyclesAcyclic(t *testing.T) {
	g := New()
	g.MergeFileGraph(makeFileGraph("/a.h", nil, []parser.Edge{
		includeEdge("/a.h", "/b.h", "/a.h"),
	}))
	g.MergeFileGraph(makeFileGraph("/b.h", nil, nil))

	cycles := g.DetectCycles()
	if len(cycles) != 0 {
		t.Fatalf("expected no cycles, got %v", cycles)
	}
}

func TestDetectCyclesTwoNode(t *testing.T) {
	g := New()
	g.MergeFileGraph(makeFileGraph("/a.h", nil, []parser.Edge{
		includeEdge("/a.h", "/b.h", "/a.h"),
	}))
	g.MergeFileGraph(makeFileGraph("/b.h", nil, []parser.Edge{
		includeEdge("/b.h", "/a.h", "/b.h"),
	}))

	cycles := g.DetectCycles()
	if len(cycles) != 1 {
		t.Fatalf("expected 1 cycle, got %d", len(cycles))
	}
	if len(cycles[0]) != 2 {
		t.Fatalf("expected 2-node cycle, got %d nodes", len(cycles[0]))
	}
}

func TestDetectCyclesThreeNode(t *testing.T) {
	g := New()
	g.MergeFileGraph(makeFileGraph("/a.h", nil, []parser.Edge{
		includeEdge("/a.h", "/b.h", "/a.h"),
	}))
	g.MergeFileGraph(makeFileGraph("/b.h", nil, []parser.Edge{
		includeEdge("/b.h", "/c.h", "/b.h"),
	}))
	g.MergeFileGraph(makeFileGraph("/c.h", nil, []parser.Edge{
		includeEdge("/c.h", "/a.h", "/c.h"),
	}))

	cycles := g.DetectCycles()
	if len(cycles) != 1 {
		t.Fatalf("expected 1 cycle, got %d", len(cycles))
	}
	if len(cycles[0]) != 3 {
		t.Fatalf("expected 3-node cycle, got %d nodes", len(cycles[0]))
	}
}

func TestDetectCyclesMixed(t *testing.T) {
	g := New()
	// Cycle: a <-> b. Acyclic: c -> a
	g.MergeFileGraph(makeFileGraph("/a.h", nil, []parser.Edge{
		includeEdge("/a.h", "/b.h", "/a.h"),
	}))
	g.MergeFileGraph(makeFileGraph("/b.h", nil, []parser.Edge{
		includeEdge("/b.h", "/a.h", "/b.h"),
	}))
	g.MergeFileGraph(makeFileGraph("/c.h", nil, []parser.Edge{
		includeEdge("/c.h", "/a.h", "/c.h"),
	}))

	cycles := g.DetectCycles()
	if len(cycles) != 1 {
		t.Fatalf("expected 1 cycle (a,b only), got %d", len(cycles))
	}
}

// --- 4.8: Orphans ---

func TestOrphans(t *testing.T) {
	g := New()
	g.MergeFileGraph(makeFileGraph("/a.cpp", []parser.Symbol{
		sym("called", parser.KindFunction, "/a.cpp"),
		sym("orphan", parser.KindFunction, "/a.cpp"),
		sym("Engine", parser.KindClass, "/a.cpp"),
	}, []parser.Edge{
		callEdge("/a.cpp:main", "/a.cpp:called", "/a.cpp"),
	}))

	orphans := g.Orphans("")
	// orphan is uncalled, called has a caller. Engine is a class (excluded by default).
	names := make(map[string]bool)
	for _, o := range orphans {
		names[o.Name] = true
	}
	if !names["orphan"] {
		t.Error("expected 'orphan' in orphans")
	}
	if names["called"] {
		t.Error("'called' should not be in orphans")
	}
	if names["Engine"] {
		t.Error("class 'Engine' should not be in default orphans")
	}
}

func TestOrphansKindFilter(t *testing.T) {
	g := New()
	g.MergeFileGraph(makeFileGraph("/a.cpp", []parser.Symbol{
		sym("foo", parser.KindFunction, "/a.cpp"),
		sym("bar", parser.KindMethod, "/a.cpp"),
	}, nil))

	orphans := g.Orphans(parser.KindFunction)
	if len(orphans) != 1 || orphans[0].Name != "foo" {
		t.Fatalf("expected only foo, got %v", orphans)
	}
}

// --- 4.9: ClassHierarchy ---

func TestClassHierarchySingle(t *testing.T) {
	g := New()
	g.MergeFileGraph(makeFileGraph("/a.cpp", []parser.Symbol{
		sym("Base", parser.KindClass, "/a.cpp"),
		sym("Derived", parser.KindClass, "/a.cpp"),
	}, []parser.Edge{
		inheritEdge("Derived", "Base", "/a.cpp"),
	}))

	h := g.ClassHierarchy("Derived")
	if h == nil {
		t.Fatal("expected hierarchy")
	}
	if len(h.Bases) != 1 || h.Bases[0].Name != "Base" {
		t.Errorf("expected base Base, got %v", h.Bases)
	}

	h = g.ClassHierarchy("Base")
	if h == nil {
		t.Fatal("expected hierarchy for Base")
	}
	if len(h.Derived) != 1 || h.Derived[0].Name != "Derived" {
		t.Errorf("expected derived Derived, got %v", h.Derived)
	}
}

func TestClassHierarchyMultiple(t *testing.T) {
	g := New()
	g.MergeFileGraph(makeFileGraph("/a.cpp", []parser.Symbol{
		sym("A", parser.KindClass, "/a.cpp"),
		sym("B", parser.KindClass, "/a.cpp"),
		sym("D", parser.KindClass, "/a.cpp"),
	}, []parser.Edge{
		inheritEdge("D", "A", "/a.cpp"),
		inheritEdge("D", "B", "/a.cpp"),
	}))

	h := g.ClassHierarchy("D")
	if h == nil {
		t.Fatal("expected hierarchy")
	}
	if len(h.Bases) != 2 {
		t.Fatalf("expected 2 bases, got %d", len(h.Bases))
	}
}

func TestClassHierarchyUnknown(t *testing.T) {
	g := New()
	h := g.ClassHierarchy("Unknown")
	if h != nil {
		t.Fatal("expected nil for unknown class")
	}
}

// --- 4.10: Coupling ---

func TestCoupling(t *testing.T) {
	g := New()
	g.MergeFileGraph(makeFileGraph("/a.cpp", []parser.Symbol{
		sym("foo", parser.KindFunction, "/a.cpp"),
	}, []parser.Edge{
		callEdge("/a.cpp:foo", "/b.cpp:bar", "/a.cpp"),
		includeEdge("/a.cpp", "utils.h", "/a.cpp"),
	}))
	g.MergeFileGraph(makeFileGraph("/b.cpp", []parser.Symbol{
		sym("bar", parser.KindFunction, "/b.cpp"),
	}, nil))

	coupling := g.Coupling("/a.cpp")
	if coupling["utils.h"] != 1 {
		t.Errorf("expected utils.h coupling 1, got %d", coupling["utils.h"])
	}
	// Cross-file call: /a.cpp:foo -> /b.cpp:bar — but the target is resolved by
	// node lookup, and the target "/b.cpp:bar" needs to be a node ID.
	// Since the edge target is "/b.cpp:bar" and that's also the node ID, it works.
	if coupling["/b.cpp"] != 1 {
		t.Errorf("expected /b.cpp coupling 1, got %d", coupling["/b.cpp"])
	}

	// Unknown file.
	coupling = g.Coupling("/unknown.cpp")
	if len(coupling) != 0 {
		t.Fatalf("expected empty coupling for unknown file, got %v", coupling)
	}
}

// --- 4.11: Concurrent access safety ---

func TestConcurrentAccess(t *testing.T) {
	g := New()

	// Pre-populate.
	for i := 0; i < 5; i++ {
		path := "/file" + string(rune('A'+i)) + ".cpp"
		name := "func" + string(rune('A'+i))
		g.MergeFileGraph(makeFileGraph(path, []parser.Symbol{
			sym(name, parser.KindFunction, path),
		}, []parser.Edge{
			callEdge(path+":"+name, "/fileA.cpp:funcA", path),
		}))
	}

	var wg sync.WaitGroup

	// 10 reader goroutines.
	for i := 0; i < 10; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			for j := 0; j < 1000; j++ {
				_ = g.Callers("/fileA.cpp:funcA", 2)
				_ = g.SearchSymbols("func", "")
				_ = g.FileSymbols("/fileA.cpp")
				_ = g.FileDependencies("/fileA.cpp")
				_ = g.Orphans("")
			}
		}()
	}

	// 2 writer goroutines.
	for i := 0; i < 2; i++ {
		wg.Add(1)
		go func(id int) {
			defer wg.Done()
			path := "/writer" + string(rune('0'+id)) + ".cpp"
			for j := 0; j < 100; j++ {
				g.MergeFileGraph(makeFileGraph(path, []parser.Symbol{
					sym("writerFunc", parser.KindFunction, path),
				}, nil))
				g.RemoveFile(path)
			}
		}(i)
	}

	wg.Wait()
}

// --- Mermaid ---

func TestMermaidCallGraph(t *testing.T) {
	g := New()
	// A calls B, B calls C
	g.MergeFileGraph(makeFileGraph("/a.cpp", []parser.Symbol{
		sym("A", parser.KindFunction, "/a.cpp"),
		sym("B", parser.KindFunction, "/a.cpp"),
		sym("C", parser.KindFunction, "/a.cpp"),
	}, []parser.Edge{
		callEdge("/a.cpp:A", "/a.cpp:B", "/a.cpp"),
		callEdge("/a.cpp:B", "/a.cpp:C", "/a.cpp"),
	}))

	diagram := g.MermaidGraph("/a.cpp:B", 1, 30)
	if diagram == "" {
		t.Fatal("expected non-empty diagram")
	}
	t.Log(diagram)

	if !strings.Contains(diagram, "graph TD") {
		t.Error("expected 'graph TD' header")
	}
	if !strings.Contains(diagram, "-->") {
		t.Error("expected edges in diagram")
	}
	// Should contain all 3 nodes since B is connected to both A and C at depth 1.
	if !strings.Contains(diagram, "\"B\"") {
		t.Error("expected center node B in diagram")
	}
}

func TestMermaidFileGraph(t *testing.T) {
	g := New()
	g.MergeFileGraph(makeFileGraph("/a.cpp", nil, []parser.Edge{
		includeEdge("/a.cpp", "/b.h", "/a.cpp"),
		includeEdge("/a.cpp", "/c.h", "/a.cpp"),
	}))

	// Need /b.h and /c.h in the files map for them to be recognized.
	g.MergeFileGraph(makeFileGraph("/b.h", nil, nil))
	g.MergeFileGraph(makeFileGraph("/c.h", nil, nil))

	diagram := g.MermaidGraph("/a.cpp", 1, 30)
	if diagram == "" {
		t.Fatal("expected non-empty diagram")
	}
	t.Log(diagram)

	if !strings.Contains(diagram, "includes") {
		t.Error("expected 'includes' edge labels in file diagram")
	}
}

func TestMermaidMaxNodes(t *testing.T) {
	g := New()
	// Create a chain of 10 functions.
	var syms []parser.Symbol
	var edges []parser.Edge
	for i := 0; i < 10; i++ {
		name := string(rune('A' + i))
		syms = append(syms, sym(name, parser.KindFunction, "/a.cpp"))
		if i > 0 {
			prev := string(rune('A' + i - 1))
			edges = append(edges, callEdge("/a.cpp:"+prev, "/a.cpp:"+name, "/a.cpp"))
		}
	}
	g.MergeFileGraph(makeFileGraph("/a.cpp", syms, edges))

	diagram := g.MermaidGraph("/a.cpp:A", 10, 5)
	if diagram == "" {
		t.Fatal("expected diagram")
	}

	// Count nodes — should be capped at ~5.
	nodeCount := strings.Count(diagram, "[\"")
	if nodeCount > 6 { // allow slight overshoot due to BFS batch
		t.Errorf("expected ~5 nodes max, got %d", nodeCount)
	}
}

func TestMermaidUnknown(t *testing.T) {
	g := New()
	diagram := g.MermaidGraph("/unknown:func", 1, 30)
	if diagram != "" {
		t.Error("expected empty diagram for unknown ID")
	}
}

// --- Stats ---

func TestStats(t *testing.T) {
	g := New()
	g.MergeFileGraph(makeFileGraph("/a.cpp", []parser.Symbol{
		sym("foo", parser.KindFunction, "/a.cpp"),
		sym("bar", parser.KindFunction, "/a.cpp"),
	}, []parser.Edge{
		callEdge("/a.cpp:foo", "/a.cpp:bar", "/a.cpp"),
		includeEdge("/a.cpp", "utils.h", "/a.cpp"),
	}))

	nodes, edges, files := g.Stats()
	if nodes != 2 {
		t.Errorf("expected 2 nodes, got %d", nodes)
	}
	if edges != 2 {
		t.Errorf("expected 2 edges, got %d", edges)
	}
	if files != 1 {
		t.Errorf("expected 1 file, got %d", files)
	}
}

// sort helper for cycle assertions
func sortedCycle(cycle []string) []string {
	sorted := make([]string, len(cycle))
	copy(sorted, cycle)
	sort.Strings(sorted)
	return sorted
}
