package graph

import (
	"fmt"
	"path/filepath"
	"strings"

	"github.com/danweinerdev/code-graph-mcp/internal/parser"
)

// DiagramEdge is a labeled edge for diagram output.
type DiagramEdge struct {
	From  string `json:"from"`
	To    string `json:"to"`
	Label string `json:"label"`
}

// DiagramResult holds the BFS traversal result for rendering.
type DiagramResult struct {
	Center string
	Edges  []DiagramEdge
}

// DiagramCallGraph performs a BFS on the call graph centered on startID and
// returns the edges found, up to depth/maxNodes limits.
func (g *Graph) DiagramCallGraph(startID string, depth, maxNodes int) *DiagramResult {
	g.mu.RLock()
	defer g.mu.RUnlock()

	if _, ok := g.nodes[startID]; !ok {
		return nil
	}

	if depth <= 0 {
		depth = 1
	}
	if maxNodes <= 0 {
		maxNodes = 30
	}

	type item struct {
		id    string
		depth int
	}

	visited := map[string]bool{startID: true}
	queue := []item{{startID, 0}}
	var rawEdges [][2]string

	for len(queue) > 0 && len(visited) < maxNodes {
		curr := queue[0]
		queue = queue[1:]
		if curr.depth >= depth {
			continue
		}

		for _, entry := range g.adj[curr.id] {
			if entry.Kind != parser.EdgeCalls {
				continue
			}
			rawEdges = append(rawEdges, [2]string{curr.id, entry.Target})
			if !visited[entry.Target] && len(visited) < maxNodes {
				visited[entry.Target] = true
				queue = append(queue, item{entry.Target, curr.depth + 1})
			}
		}

		for _, entry := range g.radj[curr.id] {
			if entry.Kind != parser.EdgeCalls {
				continue
			}
			rawEdges = append(rawEdges, [2]string{entry.Target, curr.id})
			if !visited[entry.Target] && len(visited) < maxNodes {
				visited[entry.Target] = true
				queue = append(queue, item{entry.Target, curr.depth + 1})
			}
		}
	}

	result := &DiagramResult{Center: mermaidLabel(startID, g.nodes)}
	seen := make(map[[2]string]bool)
	for _, e := range rawEdges {
		if seen[e] || !visited[e[0]] || !visited[e[1]] {
			continue
		}
		seen[e] = true
		result.Edges = append(result.Edges, DiagramEdge{
			From:  mermaidLabel(e[0], g.nodes),
			To:    mermaidLabel(e[1], g.nodes),
			Label: "calls",
		})
	}
	return result
}

// DiagramFileGraph performs a BFS on the include graph centered on startID.
func (g *Graph) DiagramFileGraph(startID string, depth, maxNodes int) *DiagramResult {
	g.mu.RLock()
	defer g.mu.RUnlock()

	if _, ok := g.files[startID]; !ok {
		return nil
	}

	if depth <= 0 {
		depth = 1
	}
	if maxNodes <= 0 {
		maxNodes = 30
	}

	type item struct {
		id    string
		depth int
	}

	visited := map[string]bool{startID: true}
	queue := []item{{startID, 0}}
	var rawEdges [][2]string

	for len(queue) > 0 && len(visited) < maxNodes {
		curr := queue[0]
		queue = queue[1:]
		if curr.depth >= depth {
			continue
		}

		for _, inc := range g.includes[curr.id] {
			rawEdges = append(rawEdges, [2]string{curr.id, inc})
			if !visited[inc] && len(visited) < maxNodes {
				visited[inc] = true
				queue = append(queue, item{inc, curr.depth + 1})
			}
		}

		for from, incs := range g.includes {
			for _, inc := range incs {
				if inc == curr.id {
					rawEdges = append(rawEdges, [2]string{from, curr.id})
					if !visited[from] && len(visited) < maxNodes {
						visited[from] = true
						queue = append(queue, item{from, curr.depth + 1})
					}
				}
			}
		}
	}

	result := &DiagramResult{Center: filepath.Base(startID)}
	seen := make(map[[2]string]bool)
	for _, e := range rawEdges {
		if seen[e] || !visited[e[0]] || !visited[e[1]] {
			continue
		}
		seen[e] = true
		result.Edges = append(result.Edges, DiagramEdge{
			From:  filepath.Base(e[0]),
			To:    filepath.Base(e[1]),
			Label: "includes",
		})
	}
	return result
}

// DiagramInheritance performs a BFS on the inheritance graph centered on className.
func (g *Graph) DiagramInheritance(className string, depth, maxNodes int) *DiagramResult {
	g.mu.RLock()
	defer g.mu.RUnlock()

	found := false
	for _, n := range g.nodes {
		if n.Symbol.Name == className &&
			(n.Symbol.Kind == parser.KindClass || n.Symbol.Kind == parser.KindStruct) {
			found = true
			break
		}
	}
	if !found {
		return nil
	}

	if depth <= 0 {
		depth = 2
	}
	if maxNodes <= 0 {
		maxNodes = 30
	}

	type item struct {
		name  string
		depth int
	}

	visited := map[string]bool{className: true}
	queue := []item{{className, 0}}
	var rawEdges [][2]string

	for len(queue) > 0 && len(visited) < maxNodes {
		curr := queue[0]
		queue = queue[1:]
		if curr.depth >= depth {
			continue
		}

		for _, entry := range g.adj[curr.name] {
			if entry.Kind != parser.EdgeInherits {
				continue
			}
			rawEdges = append(rawEdges, [2]string{curr.name, entry.Target})
			if !visited[entry.Target] && len(visited) < maxNodes {
				visited[entry.Target] = true
				queue = append(queue, item{entry.Target, curr.depth + 1})
			}
		}

		for _, entry := range g.radj[curr.name] {
			if entry.Kind != parser.EdgeInherits {
				continue
			}
			rawEdges = append(rawEdges, [2]string{entry.Target, curr.name})
			if !visited[entry.Target] && len(visited) < maxNodes {
				visited[entry.Target] = true
				queue = append(queue, item{entry.Target, curr.depth + 1})
			}
		}
	}

	result := &DiagramResult{Center: className}
	seen := make(map[[2]string]bool)
	for _, e := range rawEdges {
		if seen[e] || !visited[e[0]] || !visited[e[1]] {
			continue
		}
		seen[e] = true
		result.Edges = append(result.Edges, DiagramEdge{
			From:  e[0],
			To:    e[1],
			Label: "inherits",
		})
	}
	return result
}

// RenderMermaid converts a DiagramResult to a Mermaid flowchart string.
// If styled is true, adds CSS class definitions and highlights the center node.
func (dr *DiagramResult) RenderMermaid(direction string, styled bool) string {
	if dr == nil || len(dr.Edges) == 0 {
		return ""
	}

	if direction == "" {
		direction = "TD"
	}

	var b strings.Builder
	b.WriteString(fmt.Sprintf("graph %s\n", direction))

	// Collect unique node names.
	nodeSet := make(map[string]bool)
	for _, e := range dr.Edges {
		nodeSet[e.From] = true
		nodeSet[e.To] = true
	}

	shortIDs := make(map[string]string)
	idx := 0
	shortID := func(name string) string {
		if s, ok := shortIDs[name]; ok {
			return s
		}
		s := fmt.Sprintf("n%d", idx)
		idx++
		shortIDs[name] = s
		return s
	}

	for name := range nodeSet {
		sid := shortID(name)
		if styled && name == dr.Center {
			b.WriteString(fmt.Sprintf("    %s[\"%s\"]:::center\n", sid, name))
		} else {
			b.WriteString(fmt.Sprintf("    %s[\"%s\"]\n", sid, name))
		}
	}

	for _, e := range dr.Edges {
		if e.Label != "" {
			b.WriteString(fmt.Sprintf("    %s -->|%s| %s\n", shortID(e.From), e.Label, shortID(e.To)))
		} else {
			b.WriteString(fmt.Sprintf("    %s --> %s\n", shortID(e.From), shortID(e.To)))
		}
	}

	if styled {
		b.WriteString("    classDef center fill:#f96,stroke:#333\n")
	}

	return b.String()
}
