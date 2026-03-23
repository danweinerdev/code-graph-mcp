package parser_test

import (
	"testing"

	"github.com/danweinerdev/code-graph-mcp/internal/parser"
)

// mockParser is a trivial Parser implementation used to verify the interface.
type mockParser struct{}

func (m *mockParser) Extensions() []string { return []string{".mock"} }

func (m *mockParser) ParseFile(path string, content []byte) (*parser.FileGraph, error) {
	return &parser.FileGraph{
		Path: path,
		Symbols: []parser.Symbol{
			{
				Name: "testFunc",
				Kind: parser.KindFunction,
				File: path,
				Line: 1,
			},
		},
	}, nil
}

func (m *mockParser) Close() {}

// Compile-time interface check.
var _ parser.Parser = (*mockParser)(nil)

func TestMockParserSatisfiesInterface(t *testing.T) {
	var p parser.Parser = &mockParser{}

	exts := p.Extensions()
	if len(exts) != 1 || exts[0] != ".mock" {
		t.Fatalf("unexpected extensions: %v", exts)
	}

	fg, err := p.ParseFile("/test.mock", []byte("content"))
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if fg.Path != "/test.mock" {
		t.Fatalf("unexpected path: %s", fg.Path)
	}
	if len(fg.Symbols) != 1 || fg.Symbols[0].Name != "testFunc" {
		t.Fatalf("unexpected symbols: %v", fg.Symbols)
	}

	p.Close()
}
