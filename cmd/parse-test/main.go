// parse-test is a CLI tool for inspecting CppParser output.
// Usage: go run ./cmd/parse-test <directory>
package main

import (
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"

	"github.com/danweinerdev/code-graph-mcp/internal/lang/cpp"
	"github.com/danweinerdev/code-graph-mcp/internal/parser"
)

func main() {
	if len(os.Args) < 2 {
		fmt.Fprintf(os.Stderr, "Usage: parse-test <directory>\n")
		os.Exit(1)
	}

	dir := os.Args[1]
	info, err := os.Stat(dir)
	if err != nil || !info.IsDir() {
		fmt.Fprintf(os.Stderr, "Error: %s is not a valid directory\n", dir)
		os.Exit(1)
	}

	p, err := cpp.NewCppParser()
	if err != nil {
		fmt.Fprintf(os.Stderr, "Error initializing parser: %v\n", err)
		os.Exit(1)
	}
	defer p.Close()

	reg := parser.NewRegistry()
	_ = reg.Register(p)

	var files []string
	var graphs []*parser.FileGraph
	var warnings []string

	err = filepath.Walk(dir, func(path string, info os.FileInfo, err error) error {
		if err != nil {
			warnings = append(warnings, fmt.Sprintf("%s: %v", path, err))
			return nil
		}
		if info.IsDir() {
			return nil
		}

		pr := reg.ForFile(path)
		if pr == nil {
			return nil
		}

		absPath, _ := filepath.Abs(path)
		content, err := os.ReadFile(path)
		if err != nil {
			warnings = append(warnings, fmt.Sprintf("%s: read error: %v", path, err))
			return nil
		}

		fg, err := pr.ParseFile(absPath, content)
		if err != nil {
			warnings = append(warnings, fmt.Sprintf("%s: parse error: %v", path, err))
			return nil
		}

		files = append(files, absPath)
		graphs = append(graphs, fg)
		return nil
	})
	if err != nil {
		fmt.Fprintf(os.Stderr, "Error walking directory: %v\n", err)
		os.Exit(1)
	}

	sort.Strings(files)

	// Collect all symbols and edges.
	var allSymbols []parser.Symbol
	var allEdges []parser.Edge
	for _, fg := range graphs {
		allSymbols = append(allSymbols, fg.Symbols...)
		allEdges = append(allEdges, fg.Edges...)
	}

	// Print report.
	fmt.Printf("=== Files (%d) ===\n", len(files))
	for _, f := range files {
		fmt.Printf("  %s\n", f)
	}

	fmt.Printf("\n=== Symbols (%d) ===\n", len(allSymbols))
	for _, s := range allSymbols {
		base := filepath.Base(s.File)
		name := s.Name
		if s.Parent != "" {
			name = s.Parent + "::" + s.Name
		}
		ns := ""
		if s.Namespace != "" {
			ns = fmt.Sprintf(" ns=%s", s.Namespace)
		}
		fmt.Printf("  [%-8s] %-30s (%s:%d)%s\n", s.Kind, name, base, s.Line, ns)
	}

	fmt.Printf("\n=== Edges (%d) ===\n", len(allEdges))
	for _, e := range allEdges {
		from := shorten(e.From)
		to := e.To
		loc := ""
		if e.Line > 0 {
			loc = fmt.Sprintf(" (line %d)", e.Line)
		}
		fmt.Printf("  [%-8s] %s -> %s%s\n", e.Kind, from, to, loc)
	}

	if len(warnings) > 0 {
		fmt.Printf("\n=== Warnings (%d) ===\n", len(warnings))
		for _, w := range warnings {
			fmt.Printf("  %s\n", w)
		}
	}

	fmt.Printf("\nDone: %d files, %d symbols, %d edges, %d warnings\n",
		len(files), len(allSymbols), len(allEdges), len(warnings))
}

func shorten(path string) string {
	// If it looks like an absolute path with a colon symbol ID, shorten the path part.
	if idx := strings.LastIndex(path, ":"); idx > 0 {
		base := filepath.Base(path[:idx])
		return base + path[idx:]
	}
	return filepath.Base(path)
}
