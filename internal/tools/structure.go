package tools

import (
	"context"
	"encoding/json"
	"fmt"

	"github.com/mark3labs/mcp-go/mcp"

	"github.com/danweinerdev/code-graph-mcp/internal/parser"
)

func (t *Tools) handleDetectCycles(ctx context.Context, req mcp.CallToolRequest) (*mcp.CallToolResult, error) {
	if err := t.requireIndexed(); err != nil {
		return mcp.NewToolResultError(err.Error()), nil
	}

	cycles := t.graph.DetectCycles()
	if cycles == nil {
		cycles = [][]string{}
	}

	jsonBytes, _ := json.Marshal(cycles)
	return mcp.NewToolResultText(string(jsonBytes)), nil
}

func (t *Tools) handleGetOrphans(ctx context.Context, req mcp.CallToolRequest) (*mcp.CallToolResult, error) {
	if err := t.requireIndexed(); err != nil {
		return mcp.NewToolResultError(err.Error()), nil
	}

	var kind parser.SymbolKind
	if k, ok := req.GetArguments()["kind"].(string); ok && k != "" {
		kind = parser.SymbolKind(k)
	}

	orphans := t.graph.Orphans(kind)
	results := make([]symbolResult, len(orphans))
	for i, s := range orphans {
		results[i] = symbolToResult(s)
	}

	jsonBytes, _ := json.Marshal(results)
	return mcp.NewToolResultText(string(jsonBytes)), nil
}

func (t *Tools) handleGetClassHierarchy(ctx context.Context, req mcp.CallToolRequest) (*mcp.CallToolResult, error) {
	if err := t.requireIndexed(); err != nil {
		return mcp.NewToolResultError(err.Error()), nil
	}

	class, ok := req.GetArguments()["class"].(string)
	if !ok || class == "" {
		return mcp.NewToolResultError("'class' is required"), nil
	}

	h := t.graph.ClassHierarchy(class)
	if h == nil {
		msg := fmt.Sprintf("class not found: %q", class)
		if suggestions := t.suggestSymbols(class, 5); suggestions != "" {
			msg += fmt.Sprintf(". Did you mean: %s?", suggestions)
		}
		return mcp.NewToolResultError(msg), nil
	}

	jsonBytes, _ := json.Marshal(h)
	return mcp.NewToolResultText(string(jsonBytes)), nil
}

func (t *Tools) handleGetCoupling(ctx context.Context, req mcp.CallToolRequest) (*mcp.CallToolResult, error) {
	if err := t.requireIndexed(); err != nil {
		return mcp.NewToolResultError(err.Error()), nil
	}

	file, ok := req.GetArguments()["file"].(string)
	if !ok || file == "" {
		return mcp.NewToolResultError("'file' is required"), nil
	}

	coupling := t.graph.Coupling(file)
	jsonBytes, _ := json.Marshal(coupling)
	return mcp.NewToolResultText(string(jsonBytes)), nil
}

func (t *Tools) handleGenerateMermaid(ctx context.Context, req mcp.CallToolRequest) (*mcp.CallToolResult, error) {
	if err := t.requireIndexed(); err != nil {
		return mcp.NewToolResultError(err.Error()), nil
	}

	symbolID, _ := req.GetArguments()["symbol"].(string)
	file, _ := req.GetArguments()["file"].(string)

	if symbolID == "" && file == "" {
		return mcp.NewToolResultError("either 'symbol' or 'file' is required"), nil
	}

	startID := symbolID
	if startID == "" {
		startID = file
	}

	depth := 1
	if d, ok := req.GetArguments()["depth"].(float64); ok && d > 0 {
		depth = int(d)
	}

	maxNodes := 30
	if m, ok := req.GetArguments()["max_nodes"].(float64); ok && m > 0 {
		maxNodes = int(m)
	}

	diagram := t.graph.MermaidGraph(startID, depth, maxNodes)
	if diagram == "" {
		msg := fmt.Sprintf("no graph found for %q", startID)
		if symbolID != "" {
			if suggestions := t.suggestSymbols(symbolID, 5); suggestions != "" {
				msg += fmt.Sprintf(". Did you mean: %s?", suggestions)
			}
		}
		return mcp.NewToolResultError(msg), nil
	}

	return mcp.NewToolResultText(diagram), nil
}
