package parser

import (
	"fmt"
	"path/filepath"
	"strings"
)

// Registry maps file extensions to Parser implementations.
type Registry struct {
	parsers map[string]Parser // extension (e.g. ".cpp") → Parser
}

// NewRegistry creates an empty parser registry.
func NewRegistry() *Registry {
	return &Registry{parsers: make(map[string]Parser)}
}

// Register adds a parser for all of its declared extensions.
// Returns an error if any extension is already registered.
func (r *Registry) Register(p Parser) error {
	for _, ext := range p.Extensions() {
		ext = strings.ToLower(ext)
		if _, exists := r.parsers[ext]; exists {
			return fmt.Errorf("extension %q is already registered", ext)
		}
		r.parsers[ext] = p
	}
	return nil
}

// ForFile returns the parser for the given file path based on its extension,
// or nil if no parser is registered for that extension.
func (r *Registry) ForFile(path string) Parser {
	ext := strings.ToLower(filepath.Ext(path))
	return r.parsers[ext]
}
