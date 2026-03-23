package tools

import (
	"context"
	"encoding/json"
	"fmt"

	"github.com/mark3labs/mcp-go/mcp"
)

func (t *Tools) handleGetCallers(ctx context.Context, req mcp.CallToolRequest) (*mcp.CallToolResult, error) {
	if err := t.requireIndexed(); err != nil {
		return mcp.NewToolResultError(err.Error()), nil
	}

	symbolID, ok := req.GetArguments()["symbol"].(string)
	if !ok || symbolID == "" {
		return mcp.NewToolResultError("'symbol' is required"), nil
	}

	depth := 1
	if d, ok := req.GetArguments()["depth"].(float64); ok && d > 0 {
		depth = int(d)
	}

	callers := t.graph.Callers(symbolID, depth)
	if len(callers) == 0 {
		// Check if the symbol even exists.
		if t.graph.SymbolDetail(symbolID) == nil {
			msg := fmt.Sprintf("symbol not found: %q", symbolID)
			if suggestions := t.suggestSymbols(symbolID, 5); suggestions != "" {
				msg += fmt.Sprintf(". Did you mean: %s?", suggestions)
			}
			return mcp.NewToolResultError(msg), nil
		}
	}

	jsonBytes, _ := json.Marshal(callers)
	return mcp.NewToolResultText(string(jsonBytes)), nil
}

func (t *Tools) handleGetCallees(ctx context.Context, req mcp.CallToolRequest) (*mcp.CallToolResult, error) {
	if err := t.requireIndexed(); err != nil {
		return mcp.NewToolResultError(err.Error()), nil
	}

	symbolID, ok := req.GetArguments()["symbol"].(string)
	if !ok || symbolID == "" {
		return mcp.NewToolResultError("'symbol' is required"), nil
	}

	depth := 1
	if d, ok := req.GetArguments()["depth"].(float64); ok && d > 0 {
		depth = int(d)
	}

	callees := t.graph.Callees(symbolID, depth)
	if len(callees) == 0 {
		if t.graph.SymbolDetail(symbolID) == nil {
			msg := fmt.Sprintf("symbol not found: %q", symbolID)
			if suggestions := t.suggestSymbols(symbolID, 5); suggestions != "" {
				msg += fmt.Sprintf(". Did you mean: %s?", suggestions)
			}
			return mcp.NewToolResultError(msg), nil
		}
	}

	jsonBytes, _ := json.Marshal(callees)
	return mcp.NewToolResultText(string(jsonBytes)), nil
}

func (t *Tools) handleGetDependencies(ctx context.Context, req mcp.CallToolRequest) (*mcp.CallToolResult, error) {
	if err := t.requireIndexed(); err != nil {
		return mcp.NewToolResultError(err.Error()), nil
	}

	file, ok := req.GetArguments()["file"].(string)
	if !ok || file == "" {
		return mcp.NewToolResultError("'file' is required"), nil
	}

	deps := t.graph.FileDependencies(file)
	if deps == nil {
		deps = []string{}
	}

	jsonBytes, _ := json.Marshal(deps)
	return mcp.NewToolResultText(string(jsonBytes)), nil
}
