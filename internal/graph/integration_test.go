//go:build integration

package graph_test

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/danweinerdev/code-graph-mcp/internal/graph"
	"github.com/danweinerdev/code-graph-mcp/internal/lang/cpp"
	"github.com/danweinerdev/code-graph-mcp/internal/parser"
)

// indexTestdata walks testdata/cpp/, parses all files, and merges into a graph.
func indexTestdata(t *testing.T) *graph.Graph {
	t.Helper()

	p, err := cpp.NewCppParser()
	if err != nil {
		t.Fatalf("NewCppParser: %v", err)
	}
	defer p.Close()

	reg := parser.NewRegistry()
	if err := reg.Register(p); err != nil {
		t.Fatal(err)
	}

	g := graph.New()

	dir := filepath.Join("..", "..", "testdata", "cpp")
	err = filepath.Walk(dir, func(path string, info os.FileInfo, err error) error {
		if err != nil || info.IsDir() {
			return err
		}
		pr := reg.ForFile(path)
		if pr == nil {
			return nil
		}
		absPath, _ := filepath.Abs(path)
		content, err := os.ReadFile(path)
		if err != nil {
			return err
		}
		fg, err := pr.ParseFile(absPath, content)
		if err != nil {
			return err
		}
		g.MergeFileGraph(fg)
		return nil
	})
	if err != nil {
		t.Fatalf("walk: %v", err)
	}
	return g
}

func TestIntegrationFullPipeline(t *testing.T) {
	g := indexTestdata(t)

	nodes, edges, files := g.Stats()
	t.Logf("Indexed: %d nodes, %d edges, %d files", nodes, edges, files)

	if files != 8 {
		t.Errorf("expected 8 files, got %d", files)
	}
	if nodes < 15 {
		t.Errorf("expected at least 15 symbols, got %d", nodes)
	}
}

func TestIntegrationSearchSymbols(t *testing.T) {
	g := indexTestdata(t)

	// Search for "Engine" should find the class and methods.
	results := g.SearchSymbols("Engine", "")
	if len(results) == 0 {
		t.Fatal("expected search results for 'Engine'")
	}
	t.Logf("Search 'Engine': %d results", len(results))
	for _, s := range results {
		t.Logf("  [%s] %s (parent=%s)", s.Kind, s.Name, s.Parent)
	}

	// Search by kind.
	classes := g.SearchSymbols("", parser.KindClass)
	if len(classes) < 4 {
		t.Errorf("expected at least 4 classes, got %d", len(classes))
	}
}

func TestIntegrationCallersCallees(t *testing.T) {
	g := indexTestdata(t)

	// Find the main function's symbol ID.
	results := g.SearchSymbols("^main$", parser.KindFunction)
	if len(results) == 0 {
		t.Fatal("could not find main()")
	}
	mainID := results[0].File + ":" + results[0].Name
	t.Logf("main symbol ID: %s", mainID)

	// main() should have callees (update, render, clamp, status).
	callees := g.Callees(mainID, 1)
	t.Logf("main() callees: %d", len(callees))
	for _, c := range callees {
		t.Logf("  -> %s (line %d)", c.SymbolID, c.Line)
	}
	if len(callees) < 3 {
		t.Errorf("expected at least 3 callees from main, got %d", len(callees))
	}
}

func TestIntegrationCycleDetection(t *testing.T) {
	g := indexTestdata(t)

	cycles := g.DetectCycles()
	t.Logf("Cycles detected: %d", len(cycles))
	for i, c := range cycles {
		t.Logf("  Cycle %d: %v", i, c)
	}

	// NOTE: circular_a.h includes "circular_b.h" (raw path) but the file is
	// indexed as an absolute path. Include path resolution (raw → absolute)
	// is deferred to Phase 5's analyze_codebase. For now, raw include paths
	// don't form cycles with absolute file keys — this is expected.
	t.Log("Include path resolution needed for cycle detection on real files (Phase 5)")
}

func TestIntegrationOrphans(t *testing.T) {
	g := indexTestdata(t)

	orphans := g.Orphans("")
	t.Logf("Orphan functions: %d", len(orphans))
	orphanNames := make(map[string]bool)
	for _, o := range orphans {
		t.Logf("  [%s] %s (%s:%d)", o.Kind, o.Name, filepath.Base(o.File), o.Line)
		orphanNames[o.Name] = true
	}

	if !orphanNames["neverCalled"] {
		t.Error("expected neverCalled in orphans")
	}
	if !orphanNames["alsoOrphaned"] {
		t.Error("expected alsoOrphaned in orphans")
	}
}

func TestIntegrationInheritance(t *testing.T) {
	g := indexTestdata(t)

	h := g.ClassHierarchy("DebugEngine")
	if h == nil {
		t.Fatal("expected hierarchy for DebugEngine")
	}
	t.Logf("DebugEngine bases: %d, derived: %d", len(h.Bases), len(h.Derived))

	if len(h.Bases) != 1 {
		t.Fatalf("expected 1 base class, got %d", len(h.Bases))
	}
	if h.Bases[0].Name != "Engine" {
		t.Errorf("expected base Engine, got %s", h.Bases[0].Name)
	}
}

func TestIntegrationFileDependencies(t *testing.T) {
	g := indexTestdata(t)

	// Find engine.cpp's absolute path via a symbol defined in it.
	// SearchSymbols matches on Name, and methods have Parent set.
	// Search for "Engine" constructor — it's a method with Parent=Engine, Name=Engine.
	results := g.SearchSymbols("Engine", parser.KindMethod)
	var engineCpp string
	for _, r := range results {
		if r.Parent == "Engine" && r.Name == "Engine" {
			engineCpp = r.File
			break
		}
	}
	if engineCpp == "" {
		t.Fatal("could not find Engine constructor to determine engine.cpp path")
	}

	deps := g.FileDependencies(engineCpp)
	t.Logf("engine.cpp dependencies: %v", deps)
	if len(deps) < 2 {
		t.Errorf("expected at least 2 deps (engine.h, utils.h), got %d", len(deps))
	}
}
