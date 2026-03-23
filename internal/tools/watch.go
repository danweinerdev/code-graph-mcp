package tools

import (
	"context"
	"encoding/json"
	"fmt"
	"log"
	"os"
	"path/filepath"
	"sync"

	"github.com/fsnotify/fsnotify"
	"github.com/mark3labs/mcp-go/mcp"

	"github.com/danweinerdev/code-graph-mcp/internal/graph"
)

// watchState manages the file watcher lifecycle.
type watchState struct {
	mu      sync.Mutex
	watcher *fsnotify.Watcher
	cancel  context.CancelFunc
	active  bool
}

func (t *Tools) handleWatchStart(ctx context.Context, req mcp.CallToolRequest) (*mcp.CallToolResult, error) {
	if err := t.requireIndexed(); err != nil {
		return mcp.NewToolResultError(err.Error()), nil
	}

	t.watch.mu.Lock()
	defer t.watch.mu.Unlock()

	if t.watch.active {
		return mcp.NewToolResultError("watch mode is already active"), nil
	}

	watcher, err := fsnotify.NewWatcher()
	if err != nil {
		return mcp.NewToolResultError(fmt.Sprintf("failed to create watcher: %v", err)), nil
	}

	// Add all directories under the indexed root.
	err = filepath.Walk(t.rootPath, func(path string, info os.FileInfo, err error) error {
		if err != nil {
			return nil
		}
		if info.IsDir() {
			return watcher.Add(path)
		}
		return nil
	})
	if err != nil {
		watcher.Close()
		return mcp.NewToolResultError(fmt.Sprintf("failed to watch directories: %v", err)), nil
	}

	watchCtx, cancel := context.WithCancel(context.Background())
	t.watch.watcher = watcher
	t.watch.cancel = cancel
	t.watch.active = true

	go t.watchLoop(watchCtx, watcher)

	result := map[string]any{
		"status":    "watching",
		"root_path": t.rootPath,
	}
	jsonBytes, _ := json.Marshal(result)
	return mcp.NewToolResultText(string(jsonBytes)), nil
}

func (t *Tools) handleWatchStop(ctx context.Context, req mcp.CallToolRequest) (*mcp.CallToolResult, error) {
	t.watch.mu.Lock()
	defer t.watch.mu.Unlock()

	if !t.watch.active {
		return mcp.NewToolResultError("watch mode is not active"), nil
	}

	t.watch.cancel()
	t.watch.watcher.Close()
	t.watch.active = false

	result := map[string]any{"status": "stopped"}
	jsonBytes, _ := json.Marshal(result)
	return mcp.NewToolResultText(string(jsonBytes)), nil
}

func (t *Tools) watchLoop(ctx context.Context, watcher *fsnotify.Watcher) {
	for {
		select {
		case <-ctx.Done():
			return
		case event, ok := <-watcher.Events:
			if !ok {
				return
			}
			if event.Has(fsnotify.Write) || event.Has(fsnotify.Create) || event.Has(fsnotify.Remove) {
				t.reindexFile(event.Name, event.Has(fsnotify.Remove))
			}
		case err, ok := <-watcher.Errors:
			if !ok {
				return
			}
			log.Printf("watch error: %v", err)
		}
	}
}

func (t *Tools) reindexFile(path string, removed bool) {
	absPath, err := filepath.Abs(path)
	if err != nil {
		return
	}

	p := t.registry.ForFile(absPath)
	if p == nil {
		return // not a supported file type
	}

	if removed {
		t.graph.RemoveFile(absPath)
		return
	}

	content, err := os.ReadFile(absPath)
	if err != nil {
		return
	}

	fg, err := p.ParseFile(absPath, content)
	if err != nil {
		return
	}

	// Resolve edges using existing graph context.
	// Build a minimal file index and symbol index from the current graph.
	allFiles := t.graph.AllFilePaths()
	fileIndex := make(map[string][]string)
	for _, f := range allFiles {
		base := filepath.Base(f)
		fileIndex[base] = append(fileIndex[base], f)
	}

	allSymbols := t.graph.AllSymbols()
	symbolIndex := make(map[string][]symbolEntry)
	for _, s := range allSymbols {
		id := graph.SymbolID(s)
		entry := symbolEntry{
			id:        id,
			file:      s.File,
			namespace: s.Namespace,
			parent:    s.Parent,
		}
		symbolIndex[s.Name] = append(symbolIndex[s.Name], entry)
		if s.Parent != "" {
			symbolIndex[s.Parent+"::"+s.Name] = append(symbolIndex[s.Parent+"::"+s.Name], entry)
		}
	}

	resolveEdges(fg, fileIndex, symbolIndex)
	t.graph.MergeFileGraph(fg)
}
