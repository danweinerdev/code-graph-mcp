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

	direction, _ := req.GetArguments()["direction"].(string)

	var coupling map[string]int
	switch direction {
	case "incoming":
		coupling = t.graph.IncomingCoupling(file)
	case "outgoing", "":
		coupling = t.graph.Coupling(file)
	case "both":
		outgoing := t.graph.Coupling(file)
		incoming := t.graph.IncomingCoupling(file)
		coupling = make(map[string]int)
		for k, v := range outgoing {
			coupling[k] += v
		}
		for k, v := range incoming {
			coupling[k] += v
		}
	default:
		return mcp.NewToolResultError("'direction' must be 'incoming', 'outgoing', or 'both'"), nil
	}

	jsonBytes, _ := json.Marshal(coupling)
	return mcp.NewToolResultText(string(jsonBytes)), nil
}

func (t *Tools) handleGenerateMermaid(ctx context.Context, req mcp.CallToolRequest) (*mcp.CallToolResult, error) {
	if err := t.requireIndexed(); err != nil {
		return mcp.NewToolResultError(err.Error()), nil
	}

	symbolID, _ := req.GetArguments()["symbol"].(string)
	file, _ := req.GetArguments()["file"].(string)
	class, _ := req.GetArguments()["class"].(string)

	if symbolID == "" && file == "" && class == "" {
		return mcp.NewToolResultError("one of 'symbol', 'file', or 'class' is required"), nil
	}

	depth := 1
	if d, ok := req.GetArguments()["depth"].(float64); ok && d > 0 {
		depth = int(d)
	}

	maxNodes := 30
	if m, ok := req.GetArguments()["max_nodes"].(float64); ok && m > 0 {
		maxNodes = int(m)
	}

	var diagram string
	if class != "" {
		diagram = t.graph.MermaidInheritance(class, depth, maxNodes)
		if diagram == "" {
			msg := fmt.Sprintf("class not found: %q", class)
			if suggestions := t.suggestSymbols(class, 5); suggestions != "" {
				msg += fmt.Sprintf(". Did you mean: %s?", suggestions)
			}
			return mcp.NewToolResultError(msg), nil
		}
	} else {
		startID := symbolID
		if startID == "" {
			startID = file
		}
		diagram = t.graph.MermaidGraph(startID, depth, maxNodes)
		if diagram == "" {
			msg := fmt.Sprintf("no graph found for %q", startID)
			if symbolID != "" {
				if suggestions := t.suggestSymbols(symbolID, 5); suggestions != "" {
					msg += fmt.Sprintf(". Did you mean: %s?", suggestions)
				}
			}
			return mcp.NewToolResultError(msg), nil
		}
	}

	return mcp.NewToolResultText(diagram), nil
}
