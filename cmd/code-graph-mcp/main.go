package main

import (
	"fmt"
	"os"

	"github.com/mark3labs/mcp-go/server"

	"github.com/danweinerdev/code-graph-mcp/internal/graph"
	"github.com/danweinerdev/code-graph-mcp/internal/lang/cpp"
	"github.com/danweinerdev/code-graph-mcp/internal/parser"
	"github.com/danweinerdev/code-graph-mcp/internal/tools"
)

func main() {
	s := server.NewMCPServer(
		"code-graph",
		"0.1.0",
		server.WithToolCapabilities(false),
	)

	g := graph.New()
	reg := parser.NewRegistry()

	cppParser, err := cpp.NewCppParser()
	if err != nil {
		fmt.Fprintf(os.Stderr, "Failed to init C++ parser: %v\n", err)
		os.Exit(1)
	}
	defer cppParser.Close()

	if err := reg.Register(cppParser); err != nil {
		fmt.Fprintf(os.Stderr, "Failed to register C++ parser: %v\n", err)
		os.Exit(1)
	}

	t := tools.New(g, reg)
	t.Register(s)

	if err := server.ServeStdio(s); err != nil {
		fmt.Fprintf(os.Stderr, "Server error: %v\n", err)
		os.Exit(1)
	}
}
