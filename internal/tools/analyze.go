package tools

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"sync"

	"github.com/mark3labs/mcp-go/mcp"

	"github.com/danweinerdev/code-graph-mcp/internal/graph"
	"github.com/danweinerdev/code-graph-mcp/internal/parser"
)

type analyzeResult struct {
	Files    int      `json:"files"`
	Symbols  int      `json:"symbols"`
	Edges    int      `json:"edges"`
	RootPath string   `json:"root_path"`
	Warnings []string `json:"warnings,omitempty"`
}

func (t *Tools) handleAnalyzeCodebase(ctx context.Context, req mcp.CallToolRequest) (*mcp.CallToolResult, error) {
	if !t.indexMu.TryLock() {
		return mcp.NewToolResultError("indexing already in progress"), nil
	}
	defer t.indexMu.Unlock()

	pathRaw, ok := req.GetArguments()["path"].(string)
	if !ok || pathRaw == "" {
		return mcp.NewToolResultError("'path' is required"), nil
	}

	absPath, err := filepath.Abs(pathRaw)
	if err != nil {
		return mcp.NewToolResultError(fmt.Sprintf("invalid path: %v", err)), nil
	}

	info, err := os.Stat(absPath)
	if err != nil || !info.IsDir() {
		return mcp.NewToolResultError(fmt.Sprintf("directory does not exist: %s", absPath)), nil
	}

	// Check for force flag.
	force, _ := req.GetArguments()["force"].(bool)

	// Try loading from cache if not forced.
	if !force {
		loaded, err := t.graph.Load(absPath)
		if err == nil && loaded {
			// Check for stale files.
			stale, _ := graph.StalePaths(absPath)
			if len(stale) == 0 {
				t.rootPath = absPath
				t.indexed.Store(true)
				nodes, edges, files := t.graph.Stats()
				result := analyzeResult{
					Files:    files,
					Symbols:  nodes,
					Edges:    edges,
					RootPath: absPath,
					Warnings: []string{"loaded from cache"},
				}
				jsonBytes, _ := json.Marshal(result)
				return mcp.NewToolResultText(string(jsonBytes)), nil
			}
			// Stale files found — fall through to re-index.
		}
	}

	// Collect files to parse.
	var filePaths []string
	var warnings []string
	err = filepath.Walk(absPath, func(path string, info os.FileInfo, err error) error {
		if err != nil {
			warnings = append(warnings, fmt.Sprintf("%s: %v", path, err))
			return nil
		}
		if info.IsDir() {
			return nil
		}
		if t.registry.ForFile(path) != nil {
			abs, _ := filepath.Abs(path)
			filePaths = append(filePaths, abs)
		}
		return nil
	})
	if err != nil {
		return mcp.NewToolResultError(fmt.Sprintf("error walking directory: %v", err)), nil
	}

	if len(filePaths) == 0 {
		return mcp.NewToolResultError(fmt.Sprintf("no supported source files found in %s", absPath)), nil
	}

	// Phase 1: Parse files concurrently.
	numWorkers := runtime.NumCPU()
	if numWorkers > len(filePaths) {
		numWorkers = len(filePaths)
	}

	jobs := make(chan string, len(filePaths))
	results := make(chan *parser.FileGraph, len(filePaths))
	errs := make(chan string, len(filePaths))

	var wg sync.WaitGroup
	for i := 0; i < numWorkers; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			for path := range jobs {
				p := t.registry.ForFile(path)
				if p == nil {
					continue
				}
				content, err := os.ReadFile(path)
				if err != nil {
					errs <- fmt.Sprintf("%s: read error: %v", path, err)
					continue
				}
				fg, err := p.ParseFile(path, content)
				if err != nil {
					errs <- fmt.Sprintf("%s: parse error: %v", path, err)
					continue
				}
				results <- fg
			}
		}()
	}

	for _, path := range filePaths {
		jobs <- path
	}
	close(jobs)

	// Wait for all workers, then close result channels.
	go func() {
		wg.Wait()
		close(results)
		close(errs)
	}()

	// Collect all FileGraphs.
	var fileGraphs []*parser.FileGraph
	for fg := range results {
		fileGraphs = append(fileGraphs, fg)
	}
	for w := range errs {
		warnings = append(warnings, w)
	}

	// Build indices for resolution.
	fileIndex := buildFileIndex(filePaths)
	symbolIndex := buildSymbolIndex(fileGraphs)

	// Resolve edges in each FileGraph.
	for _, fg := range fileGraphs {
		resolveEdges(fg, fileIndex, symbolIndex)
	}

	// Phase 2: Merge into graph sequentially.
	t.graph.Clear()
	for _, fg := range fileGraphs {
		t.graph.MergeFileGraph(fg)
	}

	t.rootPath = absPath
	t.indexed.Store(true)

	// Save cache for next time.
	if err := t.graph.Save(absPath); err != nil {
		warnings = append(warnings, fmt.Sprintf("cache save failed: %v", err))
	}

	nodes, edges, files := t.graph.Stats()
	result := analyzeResult{
		Files:    files,
		Symbols:  nodes,
		Edges:    edges,
		RootPath: absPath,
		Warnings: warnings,
	}

	jsonBytes, _ := json.Marshal(result)
	return mcp.NewToolResultText(string(jsonBytes)), nil
}

// buildFileIndex maps basenames to absolute paths for include resolution.
// If multiple files share a basename, all are stored.
func buildFileIndex(paths []string) map[string][]string {
	index := make(map[string][]string)
	for _, p := range paths {
		base := filepath.Base(p)
		index[base] = append(index[base], p)
	}
	return index
}

// symbolEntry is used for call resolution.
type symbolEntry struct {
	id        string
	file      string
	namespace string
	parent    string
}

// buildSymbolIndex maps callee names to candidate symbol entries.
// For a method "Engine::update", both "Engine::update" and "update" are keys.
func buildSymbolIndex(fileGraphs []*parser.FileGraph) map[string][]symbolEntry {
	index := make(map[string][]symbolEntry)
	for _, fg := range fileGraphs {
		for _, s := range fg.Symbols {
			id := graph.SymbolID(s)
			entry := symbolEntry{
				id:        id,
				file:      s.File,
				namespace: s.Namespace,
				parent:    s.Parent,
			}

			// Index by bare name.
			index[s.Name] = append(index[s.Name], entry)

			// Also index by Parent::Name if it has a parent.
			if s.Parent != "" {
				qualified := s.Parent + "::" + s.Name
				index[qualified] = append(index[qualified], entry)
			}

			// Also by namespace::name and namespace::Parent::Name.
			if s.Namespace != "" {
				nsQualified := s.Namespace + "::" + s.Name
				index[nsQualified] = append(index[nsQualified], entry)
				if s.Parent != "" {
					full := s.Namespace + "::" + s.Parent + "::" + s.Name
					index[full] = append(index[full], entry)
				}
			}
		}
	}
	return index
}

// resolveEdges updates edges in a FileGraph with resolved symbol IDs and file paths.
func resolveEdges(fg *parser.FileGraph, fileIndex map[string][]string, symbolIndex map[string][]symbolEntry) {
	for i := range fg.Edges {
		e := &fg.Edges[i]
		switch e.Kind {
		case parser.EdgeIncludes:
			resolved := resolveInclude(e.To, fileIndex)
			if resolved != "" {
				e.To = resolved
			}
		case parser.EdgeCalls:
			resolved := resolveCall(e.To, e.From, fg.Path, symbolIndex)
			if resolved != "" {
				e.To = resolved
			}
		}
	}
}

// resolveInclude resolves a raw include path to an absolute path via basename matching.
func resolveInclude(raw string, fileIndex map[string][]string) string {
	// Try exact basename match.
	base := filepath.Base(raw)
	candidates := fileIndex[base]
	if len(candidates) == 1 {
		return candidates[0]
	}
	// Multiple candidates or none — try matching the suffix.
	if len(candidates) > 1 {
		for _, c := range candidates {
			if strings.HasSuffix(c, "/"+raw) || strings.HasSuffix(c, "\\"+raw) {
				return c
			}
		}
		// Ambiguous — return first candidate.
		return candidates[0]
	}
	return "" // unresolved (system include, etc.)
}

// resolveCall resolves a bare callee name to a symbol ID using scope heuristics.
// Priority: same file > same parent class > same namespace > any file.
func resolveCall(callee, callerID, callerFile string, symbolIndex map[string][]symbolEntry) string {
	candidates := symbolIndex[callee]
	if len(candidates) == 0 {
		return "" // unresolved
	}
	if len(candidates) == 1 {
		return candidates[0].id
	}

	// Extract caller's context for heuristic ranking.
	callerParent := ""
	callerNS := ""
	if idx := strings.LastIndex(callerID, ":"); idx > 0 {
		name := callerID[idx+1:]
		if parts := strings.Split(name, "::"); len(parts) > 1 {
			callerParent = parts[0]
		}
	}

	// Score candidates.
	var best symbolEntry
	bestScore := -1
	for _, c := range candidates {
		score := 0
		if c.file == callerFile {
			score += 4
		}
		if callerParent != "" && c.parent == callerParent {
			score += 3
		}
		if callerNS != "" && c.namespace == callerNS {
			score += 2
		}
		if score > bestScore {
			bestScore = score
			best = c
		}
	}

	_ = callerNS // avoid unused if no NS matching
	return best.id
}
