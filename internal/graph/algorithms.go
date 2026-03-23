package graph

// DetectCycles returns all strongly connected components of size > 1
// in the file-level include graph, using Tarjan's algorithm.
func (g *Graph) DetectCycles() [][]string {
	g.mu.RLock()
	defer g.mu.RUnlock()

	// Collect all file nodes that participate in includes.
	allFiles := make(map[string]bool)
	for from, tos := range g.includes {
		allFiles[from] = true
		for _, to := range tos {
			allFiles[to] = true
		}
	}

	return tarjanSCC(allFiles, g.includes)
}

// tarjanSCC implements Tarjan's strongly connected components algorithm.
// Returns SCCs of size > 1 (cycles).
func tarjanSCC(nodes map[string]bool, adj map[string][]string) [][]string {
	index := 0
	var stack []string
	onStack := make(map[string]bool)
	indices := make(map[string]int)
	lowlinks := make(map[string]int)
	visited := make(map[string]bool)
	var result [][]string

	var strongconnect func(v string)
	strongconnect = func(v string) {
		indices[v] = index
		lowlinks[v] = index
		index++
		visited[v] = true
		stack = append(stack, v)
		onStack[v] = true

		for _, w := range adj[v] {
			if !visited[w] {
				strongconnect(w)
				if lowlinks[w] < lowlinks[v] {
					lowlinks[v] = lowlinks[w]
				}
			} else if onStack[w] {
				if indices[w] < lowlinks[v] {
					lowlinks[v] = indices[w]
				}
			}
		}

		// If v is a root node, pop the SCC.
		if lowlinks[v] == indices[v] {
			var scc []string
			for {
				w := stack[len(stack)-1]
				stack = stack[:len(stack)-1]
				onStack[w] = false
				scc = append(scc, w)
				if w == v {
					break
				}
			}
			// Only report cycles (size > 1).
			if len(scc) > 1 {
				result = append(result, scc)
			}
		}
	}

	for node := range nodes {
		if !visited[node] {
			strongconnect(node)
		}
	}

	return result
}
