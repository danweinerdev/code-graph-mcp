package parser

// SymbolKind identifies the type of a code symbol.
type SymbolKind string

const (
	KindFunction SymbolKind = "function"
	KindMethod   SymbolKind = "method"
	KindClass    SymbolKind = "class"
	KindStruct   SymbolKind = "struct"
	KindEnum     SymbolKind = "enum"
	KindTypedef  SymbolKind = "typedef"
)

// EdgeKind identifies the type of a relationship between symbols or files.
type EdgeKind string

const (
	EdgeCalls    EdgeKind = "calls"
	EdgeIncludes EdgeKind = "includes"
	EdgeInherits EdgeKind = "inherits"
)

// Symbol represents a named code entity (function, class, etc.).
type Symbol struct {
	Name      string     `json:"name"`
	Kind      SymbolKind `json:"kind"`
	File      string     `json:"file"`
	Line      int        `json:"line"`
	Column    int        `json:"column"`
	EndLine   int        `json:"end_line"`
	Signature string     `json:"signature"`
	Namespace string     `json:"namespace,omitempty"`
	Parent    string     `json:"parent,omitempty"`
}

// Edge represents a relationship between symbols or files.
type Edge struct {
	From string   `json:"from"`
	To   string   `json:"to"`
	Kind EdgeKind `json:"kind"`
	File string   `json:"file"`
	Line int      `json:"line"`
}

// FileGraph is the output of parsing a single file. It contains the symbols
// defined in that file and the relationships observed.
type FileGraph struct {
	Path    string   `json:"path"`
	Symbols []Symbol `json:"symbols"`
	Edges   []Edge   `json:"edges"`
}
