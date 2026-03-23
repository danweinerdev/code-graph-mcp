package tools

import (
	"fmt"
	"sync"
	"sync/atomic"

	"github.com/mark3labs/mcp-go/mcp"
	"github.com/mark3labs/mcp-go/server"

	"github.com/danweinerdev/code-graph-mcp/internal/graph"
	"github.com/danweinerdev/code-graph-mcp/internal/parser"
)

// Tools holds the graph, parser registry, and state for MCP tool handlers.
type Tools struct {
	graph    *graph.Graph
	registry *parser.Registry
	indexed  atomic.Bool
	indexMu  sync.Mutex
	rootPath string
}

// New creates a new Tools instance.
func New(g *graph.Graph, reg *parser.Registry) *Tools {
	return &Tools{graph: g, registry: reg}
}

func (t *Tools) requireIndexed() error {
	if !t.indexed.Load() {
		return fmt.Errorf("no codebase indexed — call analyze_codebase first")
	}
	return nil
}

// Register adds all tool definitions and handlers to the MCP server.
func (t *Tools) Register(s *server.MCPServer) {
	// Indexing
	s.AddTool(mcp.NewTool("analyze_codebase",
		mcp.WithDescription("Index a C/C++ codebase and build the code graph. Must be called before any query tools."),
		mcp.WithString("path", mcp.Required(), mcp.Description("Absolute path to the directory to index")),
		mcp.WithBoolean("force", mcp.Description("Force full re-index, ignoring any cache (default false)")),
	), t.handleAnalyzeCodebase)

	// Symbol queries
	s.AddTool(mcp.NewTool("get_file_symbols",
		mcp.WithDescription("List all symbols (functions, classes, etc.) defined in a file"),
		mcp.WithString("file", mcp.Required(), mcp.Description("Absolute path to the source file")),
	), t.handleGetFileSymbols)

	s.AddTool(mcp.NewTool("search_symbols",
		mcp.WithDescription("Search for symbols by name pattern across the indexed codebase"),
		mcp.WithString("query", mcp.Required(), mcp.Description("Substring or regex pattern to match symbol names")),
		mcp.WithString("kind", mcp.Description("Filter by symbol kind: function, method, class, struct, enum, typedef")),
	), t.handleSearchSymbols)

	s.AddTool(mcp.NewTool("get_symbol_detail",
		mcp.WithDescription("Get full details for a symbol by its ID"),
		mcp.WithString("symbol", mcp.Required(), mcp.Description("Symbol ID in format file:name as returned by get_file_symbols or search_symbols")),
	), t.handleGetSymbolDetail)

	// Call graph queries
	s.AddTool(mcp.NewTool("get_callers",
		mcp.WithDescription("Find functions that call the given symbol (upstream call chain)"),
		mcp.WithString("symbol", mcp.Required(), mcp.Description("Symbol ID in format file:name")),
		mcp.WithNumber("depth", mcp.Description("Maximum traversal depth (default 1)")),
	), t.handleGetCallers)

	s.AddTool(mcp.NewTool("get_callees",
		mcp.WithDescription("Find functions called by the given symbol (downstream call chain)"),
		mcp.WithString("symbol", mcp.Required(), mcp.Description("Symbol ID in format file:name")),
		mcp.WithNumber("depth", mcp.Description("Maximum traversal depth (default 1)")),
	), t.handleGetCallees)

	// Dependency queries
	s.AddTool(mcp.NewTool("get_dependencies",
		mcp.WithDescription("List files included/imported by the given file"),
		mcp.WithString("file", mcp.Required(), mcp.Description("Absolute path to the source file")),
	), t.handleGetDependencies)

	// Structural analysis (P1)
	s.AddTool(mcp.NewTool("detect_cycles",
		mcp.WithDescription("Detect circular include dependencies in the indexed codebase"),
	), t.handleDetectCycles)

	s.AddTool(mcp.NewTool("get_orphans",
		mcp.WithDescription("Find symbols with no incoming call edges (uncalled functions/methods)"),
		mcp.WithString("kind", mcp.Description("Filter by symbol kind: function, method (default: all callables)")),
	), t.handleGetOrphans)

	s.AddTool(mcp.NewTool("get_class_hierarchy",
		mcp.WithDescription("Get the inheritance tree for a class (base classes and derived classes)"),
		mcp.WithString("class", mcp.Required(), mcp.Description("Class name to look up")),
	), t.handleGetClassHierarchy)

	s.AddTool(mcp.NewTool("get_coupling",
		mcp.WithDescription("Get cross-file dependency counts for a file"),
		mcp.WithString("file", mcp.Required(), mcp.Description("Absolute path to the source file")),
	), t.handleGetCoupling)
}

// suggestSymbols returns up to limit symbol name suggestions for did-you-mean errors.
func (t *Tools) suggestSymbols(name string, limit int) string {
	results := t.graph.SearchSymbols(name, "")
	if len(results) == 0 {
		return ""
	}
	if len(results) > limit {
		results = results[:limit]
	}
	var suggestions string
	for i, s := range results {
		if i > 0 {
			suggestions += ", "
		}
		suggestions += graph.SymbolID(s)
	}
	return suggestions
}
