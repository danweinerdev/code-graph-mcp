package tools

import (
	"encoding/json"
	"path/filepath"
	"strings"
	"testing"

	"github.com/danweinerdev/code-graph-mcp/internal/graph"
)

// analyzeProject is a helper that indexes a testdata project and returns the tools instance.
func analyzeProject(t *testing.T, project string) *Tools {
	t.Helper()
	tools := setupTools(t)
	dir, _ := filepath.Abs("../../testdata/" + project)

	result := callTool(t, tools, tools.handleAnalyzeCodebase, map[string]any{"path": dir})
	if result.IsError {
		t.Fatalf("analyze %s failed: %s", project, textContent(result))
	}

	var ar analyzeResult
	json.Unmarshal([]byte(textContent(result)), &ar)
	t.Logf("%s: %d files, %d symbols, %d edges", project, ar.Files, ar.Symbols, ar.Edges)
	return tools
}

// --- Game Project Tests ---

func TestGameProjectIndexes(t *testing.T) {
	tools := analyzeProject(t, "game")

	var ar analyzeResult
	result := callTool(t, tools, tools.handleAnalyzeCodebase, map[string]any{
		"path":  func() string { d, _ := filepath.Abs("../../testdata/game"); return d }(),
		"force": true,
	})
	json.Unmarshal([]byte(textContent(result)), &ar)

	if ar.Files != 7 {
		t.Errorf("expected 7 files, got %d", ar.Files)
	}
	if ar.Symbols < 40 {
		t.Errorf("expected at least 40 symbols, got %d", ar.Symbols)
	}
}

func TestGameInheritanceHierarchy(t *testing.T) {
	tools := analyzeProject(t, "game")

	// Player inherits from Entity.
	result := callTool(t, tools, tools.handleGetClassHierarchy, map[string]any{"class": "Player"})
	if result.IsError {
		t.Fatalf("hierarchy failed: %s", textContent(result))
	}

	var h graph.HierarchyNode
	json.Unmarshal([]byte(textContent(result)), &h)
	if len(h.Bases) == 0 {
		t.Error("expected Player to have base classes")
	} else {
		t.Logf("Player bases: %v", h.Bases[0].Name)
	}

	// Vec3 inherits from Vec2.
	result = callTool(t, tools, tools.handleGetClassHierarchy, map[string]any{"class": "Vec3"})
	if result.IsError {
		t.Fatalf("Vec3 hierarchy failed: %s", textContent(result))
	}
	json.Unmarshal([]byte(textContent(result)), &h)
	if len(h.Bases) == 0 || h.Bases[0].Name != "Vec2" {
		t.Error("expected Vec3 to inherit from Vec2")
	}
}

func TestGameOperatorOverloads(t *testing.T) {
	tools := analyzeProject(t, "game")

	result := callTool(t, tools, tools.handleSearchSymbols, map[string]any{"query": "operator"})
	if result.IsError {
		t.Fatalf("search failed: %s", textContent(result))
	}

	var symbols []symbolResult
	json.Unmarshal([]byte(textContent(result)), &symbols)
	t.Logf("Operators found: %d", len(symbols))
	for _, s := range symbols {
		t.Logf("  %s (parent=%s)", s.Name, s.Parent)
	}

	if len(symbols) < 4 {
		t.Errorf("expected at least 4 operators (+, -, *, ==), got %d", len(symbols))
	}
}

func TestGameCallGraph(t *testing.T) {
	tools := analyzeProject(t, "game")

	// Find main and check callees.
	result := callTool(t, tools, tools.handleSearchSymbols, map[string]any{"query": "^main$", "kind": "function"})
	var symbols []symbolResult
	json.Unmarshal([]byte(textContent(result)), &symbols)

	if len(symbols) == 0 {
		t.Fatal("could not find main()")
	}

	mainID := symbols[0].ID
	result = callTool(t, tools, tools.handleGetCallees, map[string]any{"symbol": mainID, "depth": 1.0})
	if result.IsError {
		t.Fatalf("callees failed: %s", textContent(result))
	}

	var callees []graph.CallChain
	json.Unmarshal([]byte(textContent(result)), &callees)
	t.Logf("main() callees: %d", len(callees))
	if len(callees) < 5 {
		t.Errorf("expected main to call at least 5 things, got %d", len(callees))
	}
}

func TestGameMermaid(t *testing.T) {
	tools := analyzeProject(t, "game")

	// Generate call graph for Player::update.
	result := callTool(t, tools, tools.handleSearchSymbols, map[string]any{"query": "Player::update", "kind": "method"})
	var symbols []symbolResult
	json.Unmarshal([]byte(textContent(result)), &symbols)

	// Find Player's update — could be in .cpp (qualified method) or .h (inline).
	var playerUpdateID string
	for _, s := range symbols {
		if s.Parent == "Player" || strings.Contains(s.ID, "player") {
			playerUpdateID = s.ID
			break
		}
	}

	if playerUpdateID == "" {
		// Fall back to any update method.
		for _, s := range symbols {
			playerUpdateID = s.ID
			break
		}
	}

	if playerUpdateID == "" {
		t.Fatal("could not find any update method")
	}

	// Default edges format.
	result = callTool(t, tools, tools.handleGenerateMermaid, map[string]any{"symbol": playerUpdateID, "depth": 2.0})
	if result.IsError {
		t.Fatalf("diagram failed: %s", textContent(result))
	}

	var edges []graph.DiagramEdge
	json.Unmarshal([]byte(textContent(result)), &edges)
	t.Logf("Player::update edges: %d", len(edges))
	if len(edges) == 0 {
		t.Error("expected edges for Player::update")
	}
}

// --- Data Structures Project Tests ---

func TestDataStructsIndexes(t *testing.T) {
	tools := analyzeProject(t, "datastructs")
	_ = tools // just verify indexing succeeds
}

func TestDataStructsNestedTypes(t *testing.T) {
	tools := analyzeProject(t, "datastructs")

	// LinkedList::Node should be a nested struct.
	result := callTool(t, tools, tools.handleSearchSymbols, map[string]any{"query": "Node"})
	if result.IsError {
		t.Fatalf("search failed: %s", textContent(result))
	}

	var symbols []symbolResult
	json.Unmarshal([]byte(textContent(result)), &symbols)

	found := false
	for _, s := range symbols {
		if s.Name == "Node" && s.Parent == "LinkedList" {
			found = true
			t.Logf("Found nested: %s (parent=%s, kind=%s)", s.Name, s.Parent, s.Kind)
		}
	}
	if !found {
		t.Error("expected LinkedList::Node as a nested struct")
	}
}

func TestDataStructsIteratorClass(t *testing.T) {
	tools := analyzeProject(t, "datastructs")

	result := callTool(t, tools, tools.handleSearchSymbols, map[string]any{"query": "Iterator"})
	var symbols []symbolResult
	json.Unmarshal([]byte(textContent(result)), &symbols)

	found := false
	for _, s := range symbols {
		if s.Name == "Iterator" {
			found = true
			t.Logf("Found: %s (parent=%s, kind=%s)", s.Name, s.Parent, s.Kind)
		}
	}
	if !found {
		t.Error("expected Iterator class")
	}
}

func TestDataStructsOperators(t *testing.T) {
	tools := analyzeProject(t, "datastructs")

	result := callTool(t, tools, tools.handleSearchSymbols, map[string]any{"query": "operator"})
	var symbols []symbolResult
	json.Unmarshal([]byte(textContent(result)), &symbols)

	t.Logf("Data structure operators: %d", len(symbols))
	if len(symbols) < 5 {
		t.Errorf("expected at least 5 operators (*, ->, ++, ==, !=), got %d", len(symbols))
	}
}

// --- Events Project Tests ---

func TestEventsIndexes(t *testing.T) {
	tools := analyzeProject(t, "events")
	_ = tools
}

func TestEventsTypedefs(t *testing.T) {
	tools := analyzeProject(t, "events")

	// Should find EventCallback (C-style typedef) and EventHandler (using alias).
	result := callTool(t, tools, tools.handleSearchSymbols, map[string]any{"query": "Event", "kind": "typedef"})
	var symbols []symbolResult
	json.Unmarshal([]byte(textContent(result)), &symbols)

	names := make(map[string]bool)
	for _, s := range symbols {
		names[s.Name] = true
		t.Logf("  typedef: %s", s.Name)
	}

	if !names["EventCallback"] {
		t.Error("expected EventCallback typedef")
	}
	if !names["EventHandler"] {
		t.Error("expected EventHandler using alias")
	}
}

func TestEventsEnumClass(t *testing.T) {
	tools := analyzeProject(t, "events")

	result := callTool(t, tools, tools.handleSearchSymbols, map[string]any{"query": "EventType"})
	var symbols []symbolResult
	json.Unmarshal([]byte(textContent(result)), &symbols)

	if len(symbols) == 0 {
		t.Fatal("expected EventType enum class")
	}
	if symbols[0].Kind != "enum" {
		t.Errorf("expected kind enum, got %s", symbols[0].Kind)
	}
}

func TestEventsInheritance(t *testing.T) {
	tools := analyzeProject(t, "events")

	// Button inherits from Widget which inherits from IEventListener.
	result := callTool(t, tools, tools.handleGetClassHierarchy, map[string]any{"class": "Button"})
	if result.IsError {
		t.Fatalf("hierarchy failed: %s", textContent(result))
	}

	var h graph.HierarchyNode
	json.Unmarshal([]byte(textContent(result)), &h)
	if len(h.Bases) == 0 {
		t.Error("expected Button to have a base class (Widget)")
	} else {
		t.Logf("Button bases: %s", h.Bases[0].Name)
	}
}

func TestEventsLambdaCallEdges(t *testing.T) {
	tools := analyzeProject(t, "events")

	// main() should have many callees including lambda invocations.
	result := callTool(t, tools, tools.handleSearchSymbols, map[string]any{"query": "^main$", "kind": "function"})
	var symbols []symbolResult
	json.Unmarshal([]byte(textContent(result)), &symbols)

	if len(symbols) == 0 {
		t.Fatal("could not find main()")
	}

	result = callTool(t, tools, tools.handleGetCallees, map[string]any{"symbol": symbols[0].ID, "depth": 1.0})
	var callees []graph.CallChain
	json.Unmarshal([]byte(textContent(result)), &callees)

	t.Logf("events main() callees: %d", len(callees))
	if len(callees) < 5 {
		t.Errorf("expected many callees from main (subscribe, publish, etc.), got %d", len(callees))
	}
}

// --- Modern C++ Project Tests ---

func TestModernIndexes(t *testing.T) {
	tools := analyzeProject(t, "modern")
	_ = tools
}

func TestModernAutoReturnTypes(t *testing.T) {
	tools := analyzeProject(t, "modern")

	// makeGreeting has trailing return type.
	result := callTool(t, tools, tools.handleSearchSymbols, map[string]any{"query": "makeGreeting"})
	var symbols []symbolResult
	json.Unmarshal([]byte(textContent(result)), &symbols)

	if len(symbols) == 0 {
		t.Fatal("expected makeGreeting function")
	}
	if symbols[0].Kind != "function" {
		t.Errorf("expected kind function, got %s", symbols[0].Kind)
	}

	// square has deduced auto return.
	result = callTool(t, tools, tools.handleSearchSymbols, map[string]any{"query": "^square$"})
	json.Unmarshal([]byte(textContent(result)), &symbols)
	if len(symbols) == 0 {
		t.Fatal("expected square function")
	}
}

func TestModernUsingAliases(t *testing.T) {
	tools := analyzeProject(t, "modern")

	result := callTool(t, tools, tools.handleSearchSymbols, map[string]any{"kind": "typedef"})
	var symbols []symbolResult
	json.Unmarshal([]byte(textContent(result)), &symbols)

	names := make(map[string]bool)
	for _, s := range symbols {
		names[s.Name] = true
	}

	t.Logf("Modern typedefs/aliases: %v", names)
	if !names["StringVec"] {
		t.Error("expected StringVec using alias")
	}
	if !names["ByteBuffer"] {
		t.Error("expected ByteBuffer using alias")
	}
}

func TestModernScopedEnums(t *testing.T) {
	tools := analyzeProject(t, "modern")

	result := callTool(t, tools, tools.handleSearchSymbols, map[string]any{"query": "LogLevel"})
	var symbols []symbolResult
	json.Unmarshal([]byte(textContent(result)), &symbols)

	if len(symbols) == 0 {
		t.Fatal("expected LogLevel enum class")
	}
	if symbols[0].Kind != "enum" {
		t.Errorf("expected kind enum, got %s", symbols[0].Kind)
	}
}

func TestModernNestedNamespace(t *testing.T) {
	tools := analyzeProject(t, "modern")

	result := callTool(t, tools, tools.handleSearchSymbols, map[string]any{"query": "Settings"})
	var symbols []symbolResult
	json.Unmarshal([]byte(textContent(result)), &symbols)

	if len(symbols) == 0 {
		t.Fatal("expected Settings struct")
	}

	// Should be in namespace config::defaults.
	t.Logf("Settings namespace: %q", symbols[0].Namespace)
	if symbols[0].Namespace != "config::defaults" {
		t.Errorf("expected namespace config::defaults, got %q", symbols[0].Namespace)
	}
}

func TestModernConstexpr(t *testing.T) {
	tools := analyzeProject(t, "modern")

	result := callTool(t, tools, tools.handleSearchSymbols, map[string]any{"query": "factorial"})
	var symbols []symbolResult
	json.Unmarshal([]byte(textContent(result)), &symbols)

	if len(symbols) == 0 {
		t.Fatal("expected factorial constexpr function")
	}
	if symbols[0].Kind != "function" {
		t.Errorf("expected kind function, got %s", symbols[0].Kind)
	}
}

func TestModernFileDependencies(t *testing.T) {
	tools := analyzeProject(t, "modern")

	// main.cpp includes pipeline.h, result.h, concepts.h.
	dir, _ := filepath.Abs("../../testdata/modern")
	mainCpp := filepath.Join(dir, "main.cpp")

	result := callTool(t, tools, tools.handleGetDependencies, map[string]any{"file": mainCpp})
	if result.IsError {
		t.Fatalf("deps failed: %s", textContent(result))
	}

	var deps []string
	json.Unmarshal([]byte(textContent(result)), &deps)
	t.Logf("main.cpp deps: %v", deps)
	if len(deps) < 3 {
		t.Errorf("expected at least 3 deps, got %d", len(deps))
	}
}
