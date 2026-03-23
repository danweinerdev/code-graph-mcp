package graph

import (
	"path/filepath"
	"regexp"
	"strings"
	"sync"

	"github.com/danweinerdev/code-graph-mcp/internal/parser"
)

// Graph is a concurrency-safe in-memory directed graph of code symbols.
type Graph struct {
	mu       sync.RWMutex
	nodes    map[string]*Node       // symbol ID → node
	adj      map[string][]EdgeEntry // from → [](to, kind, file, line)
	radj     map[string][]EdgeEntry // to → [](from, kind, file, line)
	files    map[string][]string    // file path → symbol IDs
	includes map[string][]string    // file path → included file paths
}

// Node wraps a parser.Symbol in the graph.
type Node struct {
	Symbol parser.Symbol
}

// EdgeEntry represents a directed edge in the adjacency list.
type EdgeEntry struct {
	Target string
	Kind   parser.EdgeKind
	File   string
	Line   int
}

// CallChain represents a node in a BFS traversal result.
type CallChain struct {
	SymbolID string `json:"symbol_id"`
	File     string `json:"file"`
	Line     int    `json:"line"`
	Depth    int    `json:"depth"`
}

// HierarchyNode represents a class in an inheritance tree.
type HierarchyNode struct {
	Name    string           `json:"name"`
	Bases   []*HierarchyNode `json:"bases,omitempty"`
	Derived []*HierarchyNode `json:"derived,omitempty"`
}

// New creates an empty graph with all maps initialized.
func New() *Graph {
	return &Graph{
		nodes:    make(map[string]*Node),
		adj:      make(map[string][]EdgeEntry),
		radj:     make(map[string][]EdgeEntry),
		files:    make(map[string][]string),
		includes: make(map[string][]string),
	}
}

// SymbolID generates the graph key for a symbol.
func SymbolID(s parser.Symbol) string {
	if s.Parent != "" {
		return s.File + ":" + s.Parent + "::" + s.Name
	}
	return s.File + ":" + s.Name
}

// MergeFileGraph adds or replaces all symbols and edges from a parsed file.
func (g *Graph) MergeFileGraph(fg *parser.FileGraph) {
	g.mu.Lock()
	defer g.mu.Unlock()

	// Remove stale data if file was previously indexed.
	if _, exists := g.files[fg.Path]; exists {
		g.removeFileUnsafe(fg.Path)
	}

	// Add symbols as nodes.
	var ids []string
	for _, s := range fg.Symbols {
		id := SymbolID(s)
		g.nodes[id] = &Node{Symbol: s}
		ids = append(ids, id)
	}
	g.files[fg.Path] = ids

	// Add edges.
	for _, e := range fg.Edges {
		entry := EdgeEntry{
			Target: e.To,
			Kind:   e.Kind,
			File:   e.File,
			Line:   e.Line,
		}

		switch e.Kind {
		case parser.EdgeCalls:
			g.adj[e.From] = append(g.adj[e.From], entry)
			g.radj[e.To] = append(g.radj[e.To], EdgeEntry{
				Target: e.From,
				Kind:   e.Kind,
				File:   e.File,
				Line:   e.Line,
			})
		case parser.EdgeIncludes:
			g.includes[e.From] = append(g.includes[e.From], e.To)
		case parser.EdgeInherits:
			g.adj[e.From] = append(g.adj[e.From], entry)
			g.radj[e.To] = append(g.radj[e.To], EdgeEntry{
				Target: e.From,
				Kind:   e.Kind,
				File:   e.File,
				Line:   e.Line,
			})
		}
	}
}

// RemoveFile removes all symbols and edges originating from a file.
func (g *Graph) RemoveFile(path string) {
	g.mu.Lock()
	defer g.mu.Unlock()
	g.removeFileUnsafe(path)
}

func (g *Graph) removeFileUnsafe(path string) {
	// Remove nodes.
	for _, id := range g.files[path] {
		delete(g.nodes, id)
	}

	// Remove adj/radj entries sourced from this file.
	for key, entries := range g.adj {
		filtered := entries[:0]
		for _, e := range entries {
			if e.File != path {
				filtered = append(filtered, e)
			}
		}
		if len(filtered) == 0 {
			delete(g.adj, key)
		} else {
			g.adj[key] = filtered
		}
	}
	for key, entries := range g.radj {
		filtered := entries[:0]
		for _, e := range entries {
			if e.File != path {
				filtered = append(filtered, e)
			}
		}
		if len(filtered) == 0 {
			delete(g.radj, key)
		} else {
			g.radj[key] = filtered
		}
	}

	delete(g.includes, path)
	delete(g.files, path)
}

// Clear resets the graph to empty.
func (g *Graph) Clear() {
	g.mu.Lock()
	defer g.mu.Unlock()
	g.nodes = make(map[string]*Node)
	g.adj = make(map[string][]EdgeEntry)
	g.radj = make(map[string][]EdgeEntry)
	g.files = make(map[string][]string)
	g.includes = make(map[string][]string)
}

// --- Query Methods ---

// FileSymbols returns all symbols defined in a file.
func (g *Graph) FileSymbols(path string) []parser.Symbol {
	g.mu.RLock()
	defer g.mu.RUnlock()

	ids := g.files[path]
	if len(ids) == 0 {
		return nil
	}
	result := make([]parser.Symbol, 0, len(ids))
	for _, id := range ids {
		if n, ok := g.nodes[id]; ok {
			result = append(result, n.Symbol)
		}
	}
	return result
}

// SymbolDetail returns the full symbol for a given ID, or nil if not found.
func (g *Graph) SymbolDetail(symbolID string) *parser.Symbol {
	g.mu.RLock()
	defer g.mu.RUnlock()

	if n, ok := g.nodes[symbolID]; ok {
		s := n.Symbol
		return &s
	}
	return nil
}

// SearchSymbols finds symbols matching a pattern, optionally filtered by kind.
// The pattern is tried as a regex first; if it fails to compile, it's used as
// a case-insensitive substring match. Results are capped at 100.
func (g *Graph) SearchSymbols(pattern string, kind parser.SymbolKind) []parser.Symbol {
	g.mu.RLock()
	defer g.mu.RUnlock()

	re, err := regexp.Compile("(?i)" + pattern)
	if err != nil {
		re = nil
	}

	lowerPattern := strings.ToLower(pattern)
	var result []parser.Symbol

	for _, n := range g.nodes {
		if kind != "" && n.Symbol.Kind != kind {
			continue
		}

		fullName := n.Symbol.Name
		if n.Symbol.Parent != "" {
			fullName = n.Symbol.Parent + "::" + n.Symbol.Name
		}

		matched := false
		if re != nil {
			matched = re.MatchString(fullName)
		} else {
			matched = strings.Contains(strings.ToLower(fullName), lowerPattern)
		}

		if matched {
			result = append(result, n.Symbol)
			if len(result) >= 100 {
				break
			}
		}
	}
	return result
}

// Callers returns symbols that call the given symbol, up to the given depth.
func (g *Graph) Callers(symbolID string, depth int) []CallChain {
	g.mu.RLock()
	defer g.mu.RUnlock()
	return g.bfs(symbolID, depth, g.radj, parser.EdgeCalls)
}

// Callees returns symbols called by the given symbol, up to the given depth.
func (g *Graph) Callees(symbolID string, depth int) []CallChain {
	g.mu.RLock()
	defer g.mu.RUnlock()
	return g.bfs(symbolID, depth, g.adj, parser.EdgeCalls)
}

func (g *Graph) bfs(startID string, depth int, adjacency map[string][]EdgeEntry, kind parser.EdgeKind) []CallChain {
	if depth <= 0 {
		depth = 1
	}

	type item struct {
		id    string
		depth int
	}

	visited := map[string]bool{startID: true}
	queue := []item{{startID, 0}}
	var result []CallChain

	for len(queue) > 0 {
		curr := queue[0]
		queue = queue[1:]

		if curr.depth >= depth {
			continue
		}

		for _, entry := range adjacency[curr.id] {
			if entry.Kind != kind {
				continue
			}
			if visited[entry.Target] {
				continue
			}
			visited[entry.Target] = true
			newDepth := curr.depth + 1

			cc := CallChain{
				SymbolID: entry.Target,
				File:     entry.File,
				Line:     entry.Line,
				Depth:    newDepth,
			}
			result = append(result, cc)
			queue = append(queue, item{entry.Target, newDepth})
		}
	}
	return result
}

// FileDependencies returns the files included by the given file.
func (g *Graph) FileDependencies(path string) []string {
	g.mu.RLock()
	defer g.mu.RUnlock()

	deps := g.includes[path]
	if len(deps) == 0 {
		return nil
	}
	// Return a copy.
	result := make([]string, len(deps))
	copy(result, deps)
	return result
}

// Orphans returns symbols with zero incoming call edges.
// If kind is non-empty, only symbols of that kind are returned.
// By default (kind=""), only callables (functions and methods) are returned.
func (g *Graph) Orphans(kind parser.SymbolKind) []parser.Symbol {
	g.mu.RLock()
	defer g.mu.RUnlock()

	var result []parser.Symbol
	for id, n := range g.nodes {
		// Default: only callables.
		if kind == "" {
			if n.Symbol.Kind != parser.KindFunction && n.Symbol.Kind != parser.KindMethod {
				continue
			}
		} else if n.Symbol.Kind != kind {
			continue
		}

		// Check for incoming call edges.
		hasCaller := false
		for _, entry := range g.radj[id] {
			if entry.Kind == parser.EdgeCalls {
				hasCaller = true
				break
			}
		}
		if !hasCaller {
			result = append(result, n.Symbol)
		}
	}
	return result
}

// ClassHierarchy returns an inheritance tree for the given class name.
// Returns nil if the class is not found.
func (g *Graph) ClassHierarchy(className string) *HierarchyNode {
	g.mu.RLock()
	defer g.mu.RUnlock()

	// Find the class node by name (search all nodes).
	var classID string
	for id, n := range g.nodes {
		if n.Symbol.Name == className &&
			(n.Symbol.Kind == parser.KindClass || n.Symbol.Kind == parser.KindStruct) {
			classID = id
			break
		}
	}
	if classID == "" {
		return nil
	}

	root := &HierarchyNode{Name: className}

	// Inheritance edges are keyed by class name (not symbol ID) in adj/radj.
	// Walk upward for base classes.
	for _, entry := range g.adj[className] {
		if entry.Kind == parser.EdgeInherits {
			root.Bases = append(root.Bases, &HierarchyNode{Name: entry.Target})
		}
	}

	// Walk downward for derived classes.
	for _, entry := range g.radj[className] {
		if entry.Kind == parser.EdgeInherits {
			root.Derived = append(root.Derived, &HierarchyNode{Name: entry.Target})
		}
	}

	return root
}

// Coupling returns a map of other file paths to the number of cross-file
// edges (calls + includes) originating from the given file.
func (g *Graph) Coupling(path string) map[string]int {
	g.mu.RLock()
	defer g.mu.RUnlock()

	counts := make(map[string]int)

	// Count cross-file call edges from symbols in this file.
	for _, id := range g.files[path] {
		for _, entry := range g.adj[id] {
			if entry.Kind == parser.EdgeCalls {
				// Find which file the target belongs to.
				if targetNode, ok := g.nodes[entry.Target]; ok {
					if targetNode.Symbol.File != path {
						counts[targetNode.Symbol.File]++
					}
				}
			}
		}
	}

	// Count include edges.
	for _, inc := range g.includes[path] {
		counts[inc]++
	}

	return counts
}

// IncomingCoupling returns a map of other file paths to the number of
// cross-file edges (calls + includes) pointing INTO the given file.
func (g *Graph) IncomingCoupling(path string) map[string]int {
	g.mu.RLock()
	defer g.mu.RUnlock()

	counts := make(map[string]int)

	// Count incoming call edges to symbols in this file.
	for _, id := range g.files[path] {
		for _, entry := range g.radj[id] {
			if entry.Kind == parser.EdgeCalls {
				// entry.Target is the caller's ID — find its file.
				if callerNode, ok := g.nodes[entry.Target]; ok {
					if callerNode.Symbol.File != path {
						counts[callerNode.Symbol.File]++
					}
				}
			}
		}
	}

	// Count files that include this file.
	for from, incs := range g.includes {
		if from == path {
			continue
		}
		for _, inc := range incs {
			if inc == path {
				counts[from]++
			}
		}
	}

	return counts
}

func mermaidLabel(id string, nodes map[string]*Node) string {
	if n, ok := nodes[id]; ok {
		if n.Symbol.Parent != "" {
			return n.Symbol.Parent + "::" + n.Symbol.Name
		}
		return n.Symbol.Name
	}
	// Not a symbol — might be a bare callee name. Use as-is but shorten paths.
	if filepath.IsAbs(id) {
		return filepath.Base(id)
	}
	return id
}

// AllFilePaths returns all indexed file paths.
func (g *Graph) AllFilePaths() []string {
	g.mu.RLock()
	defer g.mu.RUnlock()

	paths := make([]string, 0, len(g.files))
	for path := range g.files {
		paths = append(paths, path)
	}
	return paths
}

// AllSymbols returns all symbols in the graph.
func (g *Graph) AllSymbols() []parser.Symbol {
	g.mu.RLock()
	defer g.mu.RUnlock()

	symbols := make([]parser.Symbol, 0, len(g.nodes))
	for _, n := range g.nodes {
		symbols = append(symbols, n.Symbol)
	}
	return symbols
}

// Stats returns summary statistics about the graph.
func (g *Graph) Stats() (nodes, edges, files int) {
	g.mu.RLock()
	defer g.mu.RUnlock()

	edgeCount := 0
	for _, entries := range g.adj {
		edgeCount += len(entries)
	}
	for _, incs := range g.includes {
		edgeCount += len(incs)
	}

	return len(g.nodes), edgeCount, len(g.files)
}
