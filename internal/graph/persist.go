package graph

import (
	"encoding/json"
	"os"
	"path/filepath"

	"github.com/danweinerdev/code-graph-mcp/internal/parser"
)

const cacheFileName = ".code-graph-cache.json"

// graphCache is the serialization-safe DTO for graph persistence.
// It separates data from the sync.RWMutex which must not be serialized.
type graphCache struct {
	Version  int                        `json:"version"`
	Nodes    map[string]parser.Symbol   `json:"nodes"`
	Adj      map[string][]EdgeEntry     `json:"adj"`
	Radj     map[string][]EdgeEntry     `json:"radj"`
	Files    map[string][]string        `json:"files"`
	Includes map[string][]string        `json:"includes"`
	Mtimes   map[string]int64           `json:"mtimes"`
}

// Save persists the graph to a JSON cache file in the given directory.
func (g *Graph) Save(dir string) error {
	g.mu.RLock()
	defer g.mu.RUnlock()

	cache := graphCache{
		Version:  1,
		Nodes:    make(map[string]parser.Symbol, len(g.nodes)),
		Adj:      g.adj,
		Radj:     g.radj,
		Files:    g.files,
		Includes: g.includes,
		Mtimes:   make(map[string]int64),
	}

	for id, n := range g.nodes {
		cache.Nodes[id] = n.Symbol
	}

	// Record mtimes for indexed files.
	for path := range g.files {
		if info, err := os.Stat(path); err == nil {
			cache.Mtimes[path] = info.ModTime().UnixNano()
		}
	}

	data, err := json.Marshal(cache)
	if err != nil {
		return err
	}

	cachePath := filepath.Join(dir, cacheFileName)
	return os.WriteFile(cachePath, data, 0644)
}

// Load restores a graph from a JSON cache file. Returns false if no cache exists.
func (g *Graph) Load(dir string) (bool, error) {
	cachePath := filepath.Join(dir, cacheFileName)
	data, err := os.ReadFile(cachePath)
	if err != nil {
		if os.IsNotExist(err) {
			return false, nil
		}
		return false, err
	}

	var cache graphCache
	if err := json.Unmarshal(data, &cache); err != nil {
		return false, err
	}

	if cache.Version != 1 {
		return false, nil // incompatible version, re-index
	}

	g.mu.Lock()
	defer g.mu.Unlock()

	g.nodes = make(map[string]*Node, len(cache.Nodes))
	for id, sym := range cache.Nodes {
		g.nodes[id] = &Node{Symbol: sym}
	}
	g.adj = cache.Adj
	g.radj = cache.Radj
	g.files = cache.Files
	g.includes = cache.Includes

	// Initialize nil maps.
	if g.adj == nil {
		g.adj = make(map[string][]EdgeEntry)
	}
	if g.radj == nil {
		g.radj = make(map[string][]EdgeEntry)
	}
	if g.files == nil {
		g.files = make(map[string][]string)
	}
	if g.includes == nil {
		g.includes = make(map[string][]string)
	}

	return true, nil
}

// StalePaths returns file paths that have changed since the cache was written.
// It compares current mtimes against the cached mtimes.
func StalePaths(dir string) (stale []string, err error) {
	cachePath := filepath.Join(dir, cacheFileName)
	data, err := os.ReadFile(cachePath)
	if err != nil {
		return nil, err
	}

	var cache graphCache
	if err := json.Unmarshal(data, &cache); err != nil {
		return nil, err
	}

	for path, cachedMtime := range cache.Mtimes {
		info, err := os.Stat(path)
		if err != nil {
			stale = append(stale, path) // file deleted or inaccessible
			continue
		}
		if info.ModTime().UnixNano() != cachedMtime {
			stale = append(stale, path)
		}
	}

	return stale, nil
}

// CachePath returns the path to the cache file for a directory.
func CachePath(dir string) string {
	return filepath.Join(dir, cacheFileName)
}
