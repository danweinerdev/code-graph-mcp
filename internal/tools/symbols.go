package tools

import (
	"context"
	"encoding/json"
	"fmt"

	"github.com/mark3labs/mcp-go/mcp"

	"github.com/danweinerdev/code-graph-mcp/internal/graph"
	"github.com/danweinerdev/code-graph-mcp/internal/parser"
)

type symbolResult struct {
	ID        string `json:"id"`
	Name      string `json:"name"`
	Kind      string `json:"kind"`
	File      string `json:"file"`
	Line      int    `json:"line"`
	Column    int    `json:"column"`
	EndLine   int    `json:"end_line"`
	Signature string `json:"signature"`
	Namespace string `json:"namespace,omitempty"`
	Parent    string `json:"parent,omitempty"`
}

func symbolToResult(s parser.Symbol) symbolResult {
	return symbolResult{
		ID:        graph.SymbolID(s),
		Name:      s.Name,
		Kind:      string(s.Kind),
		File:      s.File,
		Line:      s.Line,
		Column:    s.Column,
		EndLine:   s.EndLine,
		Signature: s.Signature,
		Namespace: s.Namespace,
		Parent:    s.Parent,
	}
}

func (t *Tools) handleGetFileSymbols(ctx context.Context, req mcp.CallToolRequest) (*mcp.CallToolResult, error) {
	if err := t.requireIndexed(); err != nil {
		return mcp.NewToolResultError(err.Error()), nil
	}

	file, ok := req.GetArguments()["file"].(string)
	if !ok || file == "" {
		return mcp.NewToolResultError("'file' is required"), nil
	}

	symbols := t.graph.FileSymbols(file)
	if len(symbols) == 0 {
		return mcp.NewToolResultError(fmt.Sprintf("no symbols found in file: %s", file)), nil
	}

	results := make([]symbolResult, len(symbols))
	for i, s := range symbols {
		results[i] = symbolToResult(s)
	}

	jsonBytes, _ := json.Marshal(results)
	return mcp.NewToolResultText(string(jsonBytes)), nil
}

func (t *Tools) handleSearchSymbols(ctx context.Context, req mcp.CallToolRequest) (*mcp.CallToolResult, error) {
	if err := t.requireIndexed(); err != nil {
		return mcp.NewToolResultError(err.Error()), nil
	}

	query, _ := req.GetArguments()["query"].(string)

	var kind parser.SymbolKind
	if k, ok := req.GetArguments()["kind"].(string); ok && k != "" {
		kind = parser.SymbolKind(k)
	}

	if query == "" && kind == "" {
		return mcp.NewToolResultError("'query' is required (or provide 'kind' to list all of a type)"), nil
	}

	symbols := t.graph.SearchSymbols(query, kind)

	results := make([]symbolResult, len(symbols))
	for i, s := range symbols {
		results[i] = symbolToResult(s)
	}

	jsonBytes, _ := json.Marshal(results)
	return mcp.NewToolResultText(string(jsonBytes)), nil
}

func (t *Tools) handleGetSymbolDetail(ctx context.Context, req mcp.CallToolRequest) (*mcp.CallToolResult, error) {
	if err := t.requireIndexed(); err != nil {
		return mcp.NewToolResultError(err.Error()), nil
	}

	symbolID, ok := req.GetArguments()["symbol"].(string)
	if !ok || symbolID == "" {
		return mcp.NewToolResultError("'symbol' is required"), nil
	}

	s := t.graph.SymbolDetail(symbolID)
	if s == nil {
		msg := fmt.Sprintf("symbol not found: %q", symbolID)
		if suggestions := t.suggestSymbols(symbolID, 5); suggestions != "" {
			msg += fmt.Sprintf(". Did you mean: %s?", suggestions)
		}
		return mcp.NewToolResultError(msg), nil
	}

	result := symbolToResult(*s)
	jsonBytes, _ := json.Marshal(result)
	return mcp.NewToolResultText(string(jsonBytes)), nil
}
