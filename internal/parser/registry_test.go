package parser_test

import (
	"testing"

	"github.com/danweinerdev/code-graph-mcp/internal/parser"
)

func TestRegistryForFile(t *testing.T) {
	reg := parser.NewRegistry()
	mock := &mockParser{}

	// Override mock to return C++ extensions for this test.
	cppMock := &cppMockParser{}
	if err := reg.Register(cppMock); err != nil {
		t.Fatalf("register failed: %v", err)
	}

	// Known extension returns the parser.
	p := reg.ForFile("src/main.cpp")
	if p == nil {
		t.Fatal("expected parser for .cpp, got nil")
	}

	p = reg.ForFile("include/engine.h")
	if p == nil {
		t.Fatal("expected parser for .h, got nil")
	}

	// Unknown extension returns nil.
	p = reg.ForFile("script.py")
	if p != nil {
		t.Fatalf("expected nil for .py, got %v", p)
	}

	// No extension returns nil.
	p = reg.ForFile("Makefile")
	if p != nil {
		t.Fatalf("expected nil for no extension, got %v", p)
	}

	_ = mock // avoid unused
}

func TestRegistryDuplicateExtension(t *testing.T) {
	reg := parser.NewRegistry()
	mock1 := &cppMockParser{}
	mock2 := &cppMockParser{}

	if err := reg.Register(mock1); err != nil {
		t.Fatalf("first register failed: %v", err)
	}

	err := reg.Register(mock2)
	if err == nil {
		t.Fatal("expected error on duplicate extension registration")
	}
}

// cppMockParser pretends to handle C++ files.
type cppMockParser struct{}

func (m *cppMockParser) Extensions() []string {
	return []string{".cpp", ".cc", ".cxx", ".c", ".h", ".hpp", ".hxx"}
}

func (m *cppMockParser) ParseFile(path string, content []byte) (*parser.FileGraph, error) {
	return &parser.FileGraph{Path: path}, nil
}

func (m *cppMockParser) Close() {}
