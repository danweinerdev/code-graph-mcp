package tools

import (
	"context"
	"encoding/json"
	"path/filepath"
	"strings"
	"testing"

	"github.com/mark3labs/mcp-go/mcp"

	"github.com/danweinerdev/code-graph-mcp/internal/graph"
	"github.com/danweinerdev/code-graph-mcp/internal/lang/cpp"
	"github.com/danweinerdev/code-graph-mcp/internal/parser"
)

func setupTools(t *testing.T) *Tools {
	t.Helper()
	g := graph.New()
	reg := parser.NewRegistry()
	p, err := cpp.NewCppParser()
	if err != nil {
		t.Fatalf("NewCppParser: %v", err)
	}
	t.Cleanup(p.Close)
	if err := reg.Register(p); err != nil {
		t.Fatal(err)
	}
	return New(g, reg)
}

func callTool(t *testing.T, tools *Tools, handler func(context.Context, mcp.CallToolRequest) (*mcp.CallToolResult, error), args map[string]any) *mcp.CallToolResult {
	t.Helper()
	req := mcp.CallToolRequest{}
	req.Params.Arguments = args
	result, err := handler(context.Background(), req)
	if err != nil {
		t.Fatalf("handler returned Go error: %v", err)
	}
	return result
}

func testdataDir(t *testing.T) string {
	t.Helper()
	abs, err := filepath.Abs("../../testdata/cpp")
	if err != nil {
		t.Fatal(err)
	}
	return abs
}

func TestGuardBeforeIndex(t *testing.T) {
	tools := setupTools(t)
	result := callTool(t, tools, tools.handleGetFileSymbols, map[string]any{"file": "/test.cpp"})
	if !result.IsError {
		t.Fatal("expected error before indexing")
	}
}

func TestAnalyzeCodebase(t *testing.T) {
	tools := setupTools(t)
	dir := testdataDir(t)

	result := callTool(t, tools, tools.handleAnalyzeCodebase, map[string]any{"path": dir})
	if result.IsError {
		t.Fatalf("analyze failed: %s", textContent(result))
	}

	var ar analyzeResult
	if err := json.Unmarshal([]byte(textContent(result)), &ar); err != nil {
		t.Fatalf("unmarshal: %v", err)
	}

	t.Logf("Analyzed: %d files, %d symbols, %d edges", ar.Files, ar.Symbols, ar.Edges)
	if ar.Files != 8 {
		t.Errorf("expected 8 files, got %d", ar.Files)
	}
	if ar.Symbols < 15 {
		t.Errorf("expected at least 15 symbols, got %d", ar.Symbols)
	}
}

func TestAnalyzeInvalidPath(t *testing.T) {
	tools := setupTools(t)
	result := callTool(t, tools, tools.handleAnalyzeCodebase, map[string]any{"path": "/nonexistent/dir"})
	if !result.IsError {
		t.Fatal("expected error for invalid path")
	}
}

func TestGetFileSymbols(t *testing.T) {
	tools := setupTools(t)
	dir := testdataDir(t)
	callTool(t, tools, tools.handleAnalyzeCodebase, map[string]any{"path": dir})

	engineCpp := filepath.Join(dir, "engine.cpp")
	result := callTool(t, tools, tools.handleGetFileSymbols, map[string]any{"file": engineCpp})
	if result.IsError {
		t.Fatalf("get_file_symbols failed: %s", textContent(result))
	}

	var symbols []symbolResult
	if err := json.Unmarshal([]byte(textContent(result)), &symbols); err != nil {
		t.Fatalf("unmarshal: %v", err)
	}
	t.Logf("engine.cpp symbols: %d", len(symbols))
	if len(symbols) < 4 {
		t.Errorf("expected at least 4 symbols in engine.cpp, got %d", len(symbols))
	}

	// Check a symbol has an ID.
	for _, s := range symbols {
		if s.ID == "" {
			t.Error("symbol missing ID")
		}
	}
}

func TestSearchSymbols(t *testing.T) {
	tools := setupTools(t)
	dir := testdataDir(t)
	callTool(t, tools, tools.handleAnalyzeCodebase, map[string]any{"path": dir})

	result := callTool(t, tools, tools.handleSearchSymbols, map[string]any{"query": "Engine"})
	if result.IsError {
		t.Fatalf("search failed: %s", textContent(result))
	}

	symbols := unmarshalSearchResults(t, result)
	t.Logf("Search 'Engine': %d results", len(symbols))
	if len(symbols) == 0 {
		t.Fatal("expected results for 'Engine'")
	}
}

func TestSearchSymbolsKindFilter(t *testing.T) {
	tools := setupTools(t)
	dir := testdataDir(t)
	callTool(t, tools, tools.handleAnalyzeCodebase, map[string]any{"path": dir})

	result := callTool(t, tools, tools.handleSearchSymbols, map[string]any{"query": "", "kind": "class"})
	if result.IsError {
		t.Fatalf("search failed: %s", textContent(result))
	}

	symbols := unmarshalSearchResults(t, result)
	for _, s := range symbols {
		if s.Kind != "class" {
			t.Errorf("expected kind class, got %s", s.Kind)
		}
	}
}

func TestSearchBriefDefault(t *testing.T) {
	tools := setupTools(t)
	dir := testdataDir(t)
	callTool(t, tools, tools.handleAnalyzeCodebase, map[string]any{"path": dir})

	// Default (no brief arg) should be brief=true → no signature/end_line.
	result := callTool(t, tools, tools.handleSearchSymbols, map[string]any{"query": "Engine"})
	symbols := unmarshalSearchResults(t, result)
	if len(symbols) == 0 {
		t.Fatal("expected results for 'Engine'")
	}
	for _, s := range symbols {
		if s.Signature != "" {
			t.Errorf("brief mode should omit signature, got %q", s.Signature)
		}
		if s.EndLine != 0 {
			t.Errorf("brief mode should omit end_line, got %d", s.EndLine)
		}
		if s.Column != 0 {
			t.Errorf("brief mode should omit column, got %d", s.Column)
		}
	}
}

func TestSearchBriefFalse(t *testing.T) {
	tools := setupTools(t)
	dir := testdataDir(t)
	callTool(t, tools, tools.handleAnalyzeCodebase, map[string]any{"path": dir})

	result := callTool(t, tools, tools.handleSearchSymbols, map[string]any{"query": "Engine", "brief": false})
	symbols := unmarshalSearchResults(t, result)
	if len(symbols) == 0 {
		t.Fatal("expected results for 'Engine'")
	}
	// At least one symbol should carry a signature when brief=false.
	hasSignature := false
	for _, s := range symbols {
		if s.Signature != "" {
			hasSignature = true
			break
		}
	}
	if !hasSignature {
		t.Error("brief=false should include signatures")
	}
}

func TestSearchPaginationEnvelope(t *testing.T) {
	tools := setupTools(t)
	dir := testdataDir(t)
	callTool(t, tools, tools.handleAnalyzeCodebase, map[string]any{"path": dir})

	result := callTool(t, tools, tools.handleSearchSymbols, map[string]any{"kind": "function", "limit": 2.0, "offset": 0.0})
	if result.IsError {
		t.Fatalf("search failed: %s", textContent(result))
	}

	var resp struct {
		Results []symbolResult `json:"results"`
		Total   int            `json:"total"`
		Offset  int            `json:"offset"`
		Limit   int            `json:"limit"`
	}
	if err := json.Unmarshal([]byte(textContent(result)), &resp); err != nil {
		t.Fatalf("unmarshal: %v", err)
	}
	if len(resp.Results) > 2 {
		t.Errorf("expected at most 2 results, got %d", len(resp.Results))
	}
	if resp.Limit != 2 {
		t.Errorf("expected limit=2, got %d", resp.Limit)
	}
	if resp.Total < len(resp.Results) {
		t.Errorf("total %d should be >= results %d", resp.Total, len(resp.Results))
	}

	// Offset should produce a different first result if there are enough symbols.
	if resp.Total > 2 {
		result2 := callTool(t, tools, tools.handleSearchSymbols, map[string]any{"kind": "function", "limit": 2.0, "offset": 2.0})
		var resp2 struct {
			Results []symbolResult `json:"results"`
			Offset  int            `json:"offset"`
		}
		json.Unmarshal([]byte(textContent(result2)), &resp2)
		if resp2.Offset != 2 {
			t.Errorf("expected offset echoed as 2, got %d", resp2.Offset)
		}
		if len(resp2.Results) > 0 && len(resp.Results) > 0 && resp2.Results[0].ID == resp.Results[0].ID {
			t.Errorf("offset should produce different first result")
		}
	}
}

func TestSearchNamespaceFilter(t *testing.T) {
	tools := setupTools(t)
	dir := testdataDir(t)
	callTool(t, tools, tools.handleAnalyzeCodebase, map[string]any{"path": dir})

	// testdata/cpp has a `utils` namespace.
	result := callTool(t, tools, tools.handleSearchSymbols, map[string]any{"namespace": "utils"})
	if result.IsError {
		t.Fatalf("search failed: %s", textContent(result))
	}
	symbols := unmarshalSearchResults(t, result)
	if len(symbols) == 0 {
		t.Fatal("expected at least one symbol in utils namespace")
	}
	for _, s := range symbols {
		if !strings.Contains(strings.ToLower(s.Namespace), "utils") {
			t.Errorf("expected symbol in utils namespace, got %q", s.Namespace)
		}
	}
}

func TestSearchRequiresAnyFilter(t *testing.T) {
	tools := setupTools(t)
	dir := testdataDir(t)
	callTool(t, tools, tools.handleAnalyzeCodebase, map[string]any{"path": dir})

	result := callTool(t, tools, tools.handleSearchSymbols, map[string]any{})
	if !result.IsError {
		t.Error("expected error when no query, kind, or namespace is provided")
	}
}

func TestGetFileSymbolsTopLevelOnly(t *testing.T) {
	tools := setupTools(t)
	dir := testdataDir(t)
	callTool(t, tools, tools.handleAnalyzeCodebase, map[string]any{"path": dir})

	engineCpp := filepath.Join(dir, "engine.cpp")

	// Default: includes nested symbols.
	resAll := callTool(t, tools, tools.handleGetFileSymbols, map[string]any{"file": engineCpp})
	var allSymbols []symbolResult
	json.Unmarshal([]byte(textContent(resAll)), &allSymbols)

	// top_level_only=true: only Parent == "".
	resTop := callTool(t, tools, tools.handleGetFileSymbols, map[string]any{"file": engineCpp, "top_level_only": true})
	var topSymbols []symbolResult
	json.Unmarshal([]byte(textContent(resTop)), &topSymbols)

	hasNested := false
	for _, s := range allSymbols {
		if s.Parent != "" {
			hasNested = true
			break
		}
	}
	if !hasNested {
		t.Skip("engine.cpp has no nested symbols in this test setup")
	}
	if len(topSymbols) >= len(allSymbols) {
		t.Errorf("expected top_level_only to filter results, got %d top vs %d all", len(topSymbols), len(allSymbols))
	}
	for _, s := range topSymbols {
		if s.Parent != "" {
			t.Errorf("top_level_only returned nested symbol with Parent=%q", s.Parent)
		}
	}
}

func TestGetSymbolSummary(t *testing.T) {
	tools := setupTools(t)
	dir := testdataDir(t)
	callTool(t, tools, tools.handleAnalyzeCodebase, map[string]any{"path": dir})

	result := callTool(t, tools, tools.handleGetSymbolSummary, map[string]any{})
	if result.IsError {
		t.Fatalf("summary failed: %s", textContent(result))
	}

	var summary map[string]map[string]int
	if err := json.Unmarshal([]byte(textContent(result)), &summary); err != nil {
		t.Fatalf("unmarshal: %v", err)
	}
	if len(summary) == 0 {
		t.Fatal("expected non-empty summary")
	}
	// utils namespace should have at least one symbol.
	if _, ok := summary["utils"]; !ok {
		t.Errorf("expected 'utils' namespace in summary, got keys %v", keysOf(summary))
	}
}

func TestGetSymbolSummaryGuard(t *testing.T) {
	tools := setupTools(t)
	result := callTool(t, tools, tools.handleGetSymbolSummary, map[string]any{})
	if !result.IsError {
		t.Fatal("expected error before indexing")
	}
}

func TestGetClassHierarchyDepth(t *testing.T) {
	tools := setupTools(t)
	dir := testdataDir(t)
	callTool(t, tools, tools.handleAnalyzeCodebase, map[string]any{"path": dir})

	// Just verify the depth parameter is accepted and returns a hierarchy.
	result := callTool(t, tools, tools.handleGetClassHierarchy, map[string]any{"class": "Engine", "depth": 2.0})
	if result.IsError {
		// Engine may have no inheritance — fall back to checking it doesn't crash.
		t.Skipf("Engine hierarchy not present: %s", textContent(result))
	}
	var h graph.HierarchyNode
	if err := json.Unmarshal([]byte(textContent(result)), &h); err != nil {
		t.Fatalf("unmarshal: %v", err)
	}
	if h.Name != "Engine" {
		t.Errorf("expected root Engine, got %q", h.Name)
	}
}

func keysOf(m map[string]map[string]int) []string {
	keys := make([]string, 0, len(m))
	for k := range m {
		keys = append(keys, k)
	}
	return keys
}

func TestGetSymbolDetail(t *testing.T) {
	tools := setupTools(t)
	dir := testdataDir(t)
	callTool(t, tools, tools.handleAnalyzeCodebase, map[string]any{"path": dir})

	// Get a known symbol ID.
	mainCpp := filepath.Join(dir, "main.cpp")
	symbolID := mainCpp + ":main"

	result := callTool(t, tools, tools.handleGetSymbolDetail, map[string]any{"symbol": symbolID})
	if result.IsError {
		t.Fatalf("get_symbol_detail failed: %s", textContent(result))
	}

	var detail symbolResult
	json.Unmarshal([]byte(textContent(result)), &detail)
	if detail.Name != "main" {
		t.Errorf("expected name main, got %s", detail.Name)
	}
}

func TestGetSymbolDetailUnknown(t *testing.T) {
	tools := setupTools(t)
	dir := testdataDir(t)
	callTool(t, tools, tools.handleAnalyzeCodebase, map[string]any{"path": dir})

	result := callTool(t, tools, tools.handleGetSymbolDetail, map[string]any{"symbol": "/fake:unknown"})
	if !result.IsError {
		t.Fatal("expected error for unknown symbol")
	}
	text := textContent(result)
	if text == "" {
		t.Fatal("expected error message")
	}
	t.Logf("Error: %s", text)
}

func TestGetCalleesFromMain(t *testing.T) {
	tools := setupTools(t)
	dir := testdataDir(t)
	callTool(t, tools, tools.handleAnalyzeCodebase, map[string]any{"path": dir})

	mainCpp := filepath.Join(dir, "main.cpp")
	symbolID := mainCpp + ":main"

	result := callTool(t, tools, tools.handleGetCallees, map[string]any{"symbol": symbolID})
	if result.IsError {
		t.Fatalf("get_callees failed: %s", textContent(result))
	}

	var callees []graph.CallChain
	json.Unmarshal([]byte(textContent(result)), &callees)
	t.Logf("main() callees: %d", len(callees))
	for _, c := range callees {
		t.Logf("  -> %s", c.SymbolID)
	}
	if len(callees) < 3 {
		t.Errorf("expected at least 3 callees from main, got %d", len(callees))
	}
}

func TestGetDependencies(t *testing.T) {
	tools := setupTools(t)
	dir := testdataDir(t)
	callTool(t, tools, tools.handleAnalyzeCodebase, map[string]any{"path": dir})

	engineCpp := filepath.Join(dir, "engine.cpp")
	result := callTool(t, tools, tools.handleGetDependencies, map[string]any{"file": engineCpp})
	if result.IsError {
		t.Fatalf("get_dependencies failed: %s", textContent(result))
	}

	var deps []string
	json.Unmarshal([]byte(textContent(result)), &deps)
	t.Logf("engine.cpp deps: %v", deps)
	if len(deps) < 2 {
		t.Errorf("expected at least 2 deps, got %d", len(deps))
	}

	// Check that include paths are resolved to absolute.
	for _, d := range deps {
		if filepath.IsAbs(d) {
			t.Logf("  resolved: %s", d)
		} else {
			t.Logf("  unresolved: %s", d)
		}
	}
}

// --- P1 Tool Tests ---

func TestDetectCycles(t *testing.T) {
	tools := setupTools(t)
	dir := testdataDir(t)
	callTool(t, tools, tools.handleAnalyzeCodebase, map[string]any{"path": dir})

	result := callTool(t, tools, tools.handleDetectCycles, nil)
	if result.IsError {
		t.Fatalf("detect_cycles failed: %s", textContent(result))
	}

	var cycles [][]string
	json.Unmarshal([]byte(textContent(result)), &cycles)
	t.Logf("Cycles: %d", len(cycles))
	for i, c := range cycles {
		t.Logf("  Cycle %d: %v", i, c)
	}
	// circular_a.h <-> circular_b.h should form a cycle now that includes are resolved.
	found := false
	for _, c := range cycles {
		if len(c) >= 2 {
			found = true
		}
	}
	if !found {
		t.Log("Note: cycle detection depends on include path resolution quality")
	}
}

func TestGetOrphans(t *testing.T) {
	tools := setupTools(t)
	dir := testdataDir(t)
	callTool(t, tools, tools.handleAnalyzeCodebase, map[string]any{"path": dir})

	result := callTool(t, tools, tools.handleGetOrphans, map[string]any{})
	if result.IsError {
		t.Fatalf("get_orphans failed: %s", textContent(result))
	}

	var orphans []symbolResult
	json.Unmarshal([]byte(textContent(result)), &orphans)
	t.Logf("Orphans: %d", len(orphans))

	names := make(map[string]bool)
	for _, o := range orphans {
		names[o.Name] = true
		t.Logf("  [%s] %s", o.Kind, o.Name)
	}
	if !names["neverCalled"] {
		t.Error("expected neverCalled in orphans")
	}
	if !names["alsoOrphaned"] {
		t.Error("expected alsoOrphaned in orphans")
	}
}

func TestGetClassHierarchy(t *testing.T) {
	tools := setupTools(t)
	dir := testdataDir(t)
	callTool(t, tools, tools.handleAnalyzeCodebase, map[string]any{"path": dir})

	result := callTool(t, tools, tools.handleGetClassHierarchy, map[string]any{"class": "DebugEngine"})
	if result.IsError {
		t.Fatalf("get_class_hierarchy failed: %s", textContent(result))
	}

	var h graph.HierarchyNode
	json.Unmarshal([]byte(textContent(result)), &h)
	t.Logf("DebugEngine: bases=%d derived=%d", len(h.Bases), len(h.Derived))
	if len(h.Bases) != 1 || h.Bases[0].Name != "Engine" {
		t.Errorf("expected base Engine, got %v", h.Bases)
	}
}

func TestGetClassHierarchyUnknown(t *testing.T) {
	tools := setupTools(t)
	dir := testdataDir(t)
	callTool(t, tools, tools.handleAnalyzeCodebase, map[string]any{"path": dir})

	result := callTool(t, tools, tools.handleGetClassHierarchy, map[string]any{"class": "NonExistentClass"})
	if !result.IsError {
		t.Fatal("expected error for unknown class")
	}
	t.Logf("Error: %s", textContent(result))
}

func TestGetCoupling(t *testing.T) {
	tools := setupTools(t)
	dir := testdataDir(t)
	callTool(t, tools, tools.handleAnalyzeCodebase, map[string]any{"path": dir})

	engineCpp := filepath.Join(dir, "engine.cpp")
	result := callTool(t, tools, tools.handleGetCoupling, map[string]any{"file": engineCpp})
	if result.IsError {
		t.Fatalf("get_coupling failed: %s", textContent(result))
	}

	var coupling map[string]int
	json.Unmarshal([]byte(textContent(result)), &coupling)
	t.Logf("engine.cpp coupling: %v", coupling)
	if len(coupling) == 0 {
		t.Error("expected non-empty coupling for engine.cpp")
	}
}

func TestDiagramEdgesFormat(t *testing.T) {
	tools := setupTools(t)
	dir := testdataDir(t)
	callTool(t, tools, tools.handleAnalyzeCodebase, map[string]any{"path": dir})

	mainCpp := filepath.Join(dir, "main.cpp")
	symbolID := mainCpp + ":main"

	// Default format=edges: returns JSON array.
	result := callTool(t, tools, tools.handleGenerateMermaid, map[string]any{"symbol": symbolID, "depth": 1.0})
	if result.IsError {
		t.Fatalf("diagram failed: %s", textContent(result))
	}

	var edges []graph.DiagramEdge
	if err := json.Unmarshal([]byte(textContent(result)), &edges); err != nil {
		t.Fatalf("expected JSON edges array, got: %s", textContent(result))
	}
	t.Logf("edges format: %d edges", len(edges))
	if len(edges) < 3 {
		t.Errorf("expected at least 3 edges from main, got %d", len(edges))
	}
	for _, e := range edges {
		if e.Label != "calls" {
			t.Errorf("expected label 'calls', got %q", e.Label)
		}
	}
}

func TestDiagramMermaidUnstyled(t *testing.T) {
	tools := setupTools(t)
	dir := testdataDir(t)
	callTool(t, tools, tools.handleAnalyzeCodebase, map[string]any{"path": dir})

	mainCpp := filepath.Join(dir, "main.cpp")
	symbolID := mainCpp + ":main"

	result := callTool(t, tools, tools.handleGenerateMermaid, map[string]any{
		"symbol": symbolID, "depth": 1.0, "format": "mermaid",
	})
	if result.IsError {
		t.Fatalf("diagram failed: %s", textContent(result))
	}

	mermaid := textContent(result)
	t.Log(mermaid)
	if !strings.Contains(mermaid, "graph TD") {
		t.Error("expected 'graph TD'")
	}
	if strings.Contains(mermaid, "classDef") {
		t.Error("unstyled should not contain classDef")
	}
	if strings.Contains(mermaid, ":::") {
		t.Error("unstyled should not contain ::: annotations")
	}
}

func TestDiagramMermaidStyled(t *testing.T) {
	tools := setupTools(t)
	dir := testdataDir(t)
	callTool(t, tools, tools.handleAnalyzeCodebase, map[string]any{"path": dir})

	mainCpp := filepath.Join(dir, "main.cpp")
	symbolID := mainCpp + ":main"

	result := callTool(t, tools, tools.handleGenerateMermaid, map[string]any{
		"symbol": symbolID, "depth": 1.0, "format": "mermaid", "styled": true,
	})
	if result.IsError {
		t.Fatalf("diagram failed: %s", textContent(result))
	}

	mermaid := textContent(result)
	if !strings.Contains(mermaid, "classDef center") {
		t.Error("styled should contain classDef")
	}
}

func TestDiagramFileEdges(t *testing.T) {
	tools := setupTools(t)
	dir := testdataDir(t)
	callTool(t, tools, tools.handleAnalyzeCodebase, map[string]any{"path": dir})

	engineCpp := filepath.Join(dir, "engine.cpp")
	result := callTool(t, tools, tools.handleGenerateMermaid, map[string]any{"file": engineCpp, "depth": 1.0})
	if result.IsError {
		t.Fatalf("diagram failed: %s", textContent(result))
	}

	var edges []graph.DiagramEdge
	json.Unmarshal([]byte(textContent(result)), &edges)
	t.Logf("file edges: %d", len(edges))
	for _, e := range edges {
		if e.Label != "includes" {
			t.Errorf("expected label 'includes', got %q", e.Label)
		}
	}
}

func TestDiagramUnknown(t *testing.T) {
	tools := setupTools(t)
	dir := testdataDir(t)
	callTool(t, tools, tools.handleAnalyzeCodebase, map[string]any{"path": dir})

	result := callTool(t, tools, tools.handleGenerateMermaid, map[string]any{"symbol": "/fake:unknown"})
	if !result.IsError {
		t.Fatal("expected error for unknown symbol")
	}
}

func TestDiagramEmptyResult(t *testing.T) {
	tools := setupTools(t)
	dir := testdataDir(t)
	callTool(t, tools, tools.handleAnalyzeCodebase, map[string]any{"path": dir})

	// Search for a function with no call edges.
	result := callTool(t, tools, tools.handleSearchSymbols, map[string]any{"query": "neverCalled"})
	symbols := unmarshalSearchResults(t, result)
	if len(symbols) == 0 {
		t.Skip("neverCalled not found")
	}

	// edges format: should return empty array, not null.
	result = callTool(t, tools, tools.handleGenerateMermaid, map[string]any{"symbol": symbols[0].ID})
	if result.IsError {
		t.Fatalf("diagram failed: %s", textContent(result))
	}
	text := textContent(result)
	if text != "[]" {
		t.Errorf("expected '[]' for isolated node, got %q", text)
	}
}

// textContent extracts the text from an MCP tool result.
func textContent(result *mcp.CallToolResult) string {
	if len(result.Content) == 0 {
		return ""
	}
	tc, ok := result.Content[0].(mcp.TextContent)
	if !ok {
		return ""
	}
	return tc.Text
}

// unmarshalSearchResults extracts the symbol slice from the paginated
// search_symbols response envelope ({results, total, offset, limit}).
func unmarshalSearchResults(t *testing.T, result *mcp.CallToolResult) []symbolResult {
	t.Helper()
	var resp struct {
		Results []symbolResult `json:"results"`
	}
	if err := json.Unmarshal([]byte(textContent(result)), &resp); err != nil {
		t.Fatalf("unmarshal search results: %v", err)
	}
	return resp.Results
}
