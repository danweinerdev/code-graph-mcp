// Package models — additional types exercising:
//   - Interface embedding interface AND a method element on the same
//     interface (Repo already does the embedding alone in repo.go;
//     this exercises the combined form).
//   - Anonymous struct field at top-level (Cluster.Endpoints).
//   - A blank-identifier function declaration (`func _() {}`) — Go
//     accepts this; tree-sitter-go parses it as `function_declaration`
//     with name=`_`. Our extractor produces a Function symbol with
//     name="_".
//
// Symbol contract for `models/deps.go` (asserted by `MANIFEST.md`):
//   Structs:    `Cluster`, `Node` (2)
//   Interfaces: `Reader`, `ReadWriter` (2)
//   Methods:    `Cluster.Add`, `Node.ID` (2)
//   Functions:  `_` (1, blank-identifier name)
//
// Calls inside this file (1 Calls edge total):
//   - `Cluster.Add -> append` (1, builtin)
//
// Imports (none).
package models

// Reader is a single-method interface.
type Reader interface {
	Read() ([]byte, error)
}

// ReadWriter embeds Reader (interface-embedding-interface) and adds
// Write — exercising the combined embedded-interface + method-element
// form. The embedded `Reader` produces no Symbol and no edge; only
// the surrounding `ReadWriter` interface is emitted.
type ReadWriter interface {
	Reader
	Write([]byte) (int, error)
}

// Node is a small struct used by Cluster.
type Node struct {
	id string
}

// ID returns the node's identifier. Pointer-receiver method.
func (n *Node) ID() string {
	return n.id
}

// Cluster has an anonymous struct slice field (Endpoints). The
// anonymous struct's inner fields are NOT emitted as nested Symbols.
type Cluster struct {
	Endpoints []struct {
		host string
		port int
	}
	nodes []*Node
}

// Add appends a node. Single direct Calls edge to the builtin `append`.
func (c *Cluster) Add(n *Node) {
	c.nodes = append(c.nodes, n)
}

// _ is a blank-identifier function declaration. Go accepts this and
// treats the function as unreferenceable; our extractor still produces
// a Function symbol with name="_". Anti-regression for the "blank
// identifier function name" edge case.
func _() {}
