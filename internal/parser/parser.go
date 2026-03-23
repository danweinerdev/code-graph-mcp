package parser

// Parser extracts symbols and relationships from source files.
type Parser interface {
	// Extensions returns file extensions this parser handles (e.g. [".cpp", ".cc", ".h"]).
	Extensions() []string

	// ParseFile parses a single file and returns its symbols and relationships.
	// content is the raw file bytes; path is the absolute file path.
	ParseFile(path string, content []byte) (*FileGraph, error)

	// Close releases any resources held by the parser.
	Close()
}
