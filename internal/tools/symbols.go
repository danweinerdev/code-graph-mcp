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
	Column    int    `json:"column,omitempty"`
	EndLine   int    `json:"end_line,omitempty"`
	Signature string `json:"signature,omitempty"`
	Namespace string `json:"namespace,omitempty"`
	Parent    string `json:"parent,omitempty"`
}

func symbolToResult(s parser.Symbol, brief bool) symbolResult {
	r := symbolResult{
		ID:        graph.SymbolID(s),
		Name:      s.Name,
		Kind:      string(s.Kind),
		File:      s.File,
		Line:      s.Line,
		Namespace: s.Namespace,
		Parent:    s.Parent,
	}
	if !brief {
		r.Column = s.Column
		r.EndLine = s.EndLine
		r.Signature = s.Signature
	}
	return r
}

func (t *Tools) handleGetFileSymbols(ctx context.Context, req mcp.CallToolRequest) (*mcp.CallToolResult, error) {
	if err := t.requireIndexed(); err != nil {
		return mcp.NewToolResultError(err.Error()), nil
	}

	file, ok := req.GetArguments()["file"].(string)
	if !ok || file == "" {
		return mcp.NewToolResultError("'file' is required"), nil
	}

	topLevelOnly, _ := req.GetArguments()["top_level_only"].(bool)

	// Default brief=true for LLM-optimized output.
	brief := true
	if b, ok := req.GetArguments()["brief"].(bool); ok {
		brief = b
	}

	symbols := t.graph.FileSymbols(file)
	if len(symbols) == 0 {
		return mcp.NewToolResultError(fmt.Sprintf("no symbols found in file: %s", file)), nil
	}

	results := make([]symbolResult, 0, len(symbols))
	for _, s := range symbols {
		if topLevelOnly && s.Parent != "" {
			continue
		}
		results = append(results, symbolToResult(s, brief))
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

	namespace, _ := req.GetArguments()["namespace"].(string)

	if query == "" && kind == "" && namespace == "" {
		return mcp.NewToolResultError("'query', 'kind', or 'namespace' is required"), nil
	}

	limit := 20
	if l, ok := req.GetArguments()["limit"].(float64); ok && l > 0 {
		limit = int(l)
	}

	offset := 0
	if o, ok := req.GetArguments()["offset"].(float64); ok && o > 0 {
		offset = int(o)
	}

	// Default brief=true for LLM-optimized output.
	brief := true
	if b, ok := req.GetArguments()["brief"].(bool); ok {
		brief = b
	}

	sr := t.graph.Search(graph.SearchParams{
		Pattern:   query,
		Kind:      kind,
		Namespace: namespace,
		Limit:     limit,
		Offset:    offset,
	})

	results := make([]symbolResult, len(sr.Symbols))
	for i, s := range sr.Symbols {
		results[i] = symbolToResult(s, brief)
	}

	response := struct {
		Results []symbolResult `json:"results"`
		Total   int            `json:"total"`
		Offset  int            `json:"offset"`
		Limit   int            `json:"limit"`
	}{
		Results: results,
		Total:   sr.Total,
		Offset:  offset,
		Limit:   limit,
	}

	jsonBytes, _ := json.Marshal(response)
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

	result := symbolToResult(*s, false) // always full detail
	jsonBytes, _ := json.Marshal(result)
	return mcp.NewToolResultText(string(jsonBytes)), nil
}

func (t *Tools) handleGetSymbolSummary(ctx context.Context, req mcp.CallToolRequest) (*mcp.CallToolResult, error) {
	if err := t.requireIndexed(); err != nil {
		return mcp.NewToolResultError(err.Error()), nil
	}

	file, _ := req.GetArguments()["file"].(string)

	summary := t.graph.SymbolSummary(file)
	jsonBytes, _ := json.Marshal(summary)
	return mcp.NewToolResultText(string(jsonBytes)), nil
}
