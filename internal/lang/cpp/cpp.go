package cpp

import (
	"strings"

	"github.com/danweinerdev/code-graph-mcp/internal/parser"

	tree_sitter "github.com/tree-sitter/go-tree-sitter"
	tree_sitter_cpp "github.com/tree-sitter/tree-sitter-cpp/bindings/go"
)

// Compile-time interface check.
var _ parser.Parser = (*CppParser)(nil)

// CppParser extracts symbols and relationships from C/C++ source files
// using tree-sitter with the C++ grammar.
type CppParser struct {
	language  *tree_sitter.Language
	defQuery  *tree_sitter.Query
	callQuery *tree_sitter.Query
	inclQuery *tree_sitter.Query
	inhQuery  *tree_sitter.Query
}

// NewCppParser creates a CppParser, compiling all tree-sitter queries against
// the C++ grammar. Returns an error if any query fails to compile.
func NewCppParser() (*CppParser, error) {
	lang := tree_sitter.NewLanguage(tree_sitter_cpp.Language())

	defQuery, err := tree_sitter.NewQuery(lang, definitionQueries)
	if err != nil {
		return nil, err
	}
	callQuery, err := tree_sitter.NewQuery(lang, callQueries)
	if err != nil {
		defQuery.Close()
		return nil, err
	}
	inclQuery, err := tree_sitter.NewQuery(lang, includeQueries)
	if err != nil {
		defQuery.Close()
		callQuery.Close()
		return nil, err
	}
	inhQuery, err := tree_sitter.NewQuery(lang, inheritanceQueries)
	if err != nil {
		defQuery.Close()
		callQuery.Close()
		inclQuery.Close()
		return nil, err
	}

	return &CppParser{
		language:  lang,
		defQuery:  defQuery,
		callQuery: callQuery,
		inclQuery: inclQuery,
		inhQuery:  inhQuery,
	}, nil
}

func (p *CppParser) Extensions() []string {
	return []string{".cpp", ".cc", ".cxx", ".c", ".h", ".hpp", ".hxx"}
}

func (p *CppParser) ParseFile(path string, content []byte) (*parser.FileGraph, error) {
	ts := tree_sitter.NewParser()
	defer ts.Close()
	ts.SetLanguage(p.language)

	tree := ts.Parse(content, nil)
	defer tree.Close()

	root := tree.RootNode()
	fg := &parser.FileGraph{Path: path}

	p.extractDefinitions(root, content, path, fg)
	p.extractCalls(root, content, path, fg)
	p.extractIncludes(root, content, path, fg)
	p.extractInheritance(root, content, path, fg)

	return fg, nil
}

func (p *CppParser) Close() {
	p.defQuery.Close()
	p.callQuery.Close()
	p.inclQuery.Close()
	p.inhQuery.Close()
}

// extractDefinitions finds function, method, class, struct, enum, and typedef
// definitions, populating fg.Symbols.
func (p *CppParser) extractDefinitions(root *tree_sitter.Node, content []byte, path string, fg *parser.FileGraph) {
	cursor := tree_sitter.NewQueryCursor()
	defer cursor.Close()

	matches := cursor.Matches(p.defQuery, root, content)
	for {
		match := matches.Next()
		if match == nil {
			break
		}

		for _, capture := range match.Captures {
			node := capture.Node
			if node.HasError() {
				continue
			}

			capName := p.captureNameForIndex(p.defQuery, capture.Index)
			text := node.Utf8Text(content)
			nodePtr := &capture.Node

			switch capName {
			case "func.name":
				defNode := findEnclosingKind(nodePtr, "function_definition")
				if defNode == nil {
					continue
				}
				ns := resolveNamespace(nodePtr, content)
				parentClass := resolveParentClass(nodePtr, content)
				kind := parser.KindFunction
				if parentClass != "" {
					kind = parser.KindMethod
				}
				fg.Symbols = append(fg.Symbols, parser.Symbol{
					Name:      text,
					Kind:      kind,
					File:      path,
					Line:      int(defNode.StartPosition().Row) + 1,
					Column:    int(defNode.StartPosition().Column),
					EndLine:   int(defNode.EndPosition().Row) + 1,
					Signature: truncateSignature(defNode.Utf8Text(content)),
					Namespace: ns,
					Parent:    parentClass,
				})

			case "method.qname":
				defNode := findEnclosingKind(nodePtr, "function_definition")
				if defNode == nil {
					continue
				}
				parent, methodName := splitQualified(text)
				ns := resolveNamespace(nodePtr, content)
				fg.Symbols = append(fg.Symbols, parser.Symbol{
					Name:      methodName,
					Kind:      parser.KindMethod,
					File:      path,
					Line:      int(defNode.StartPosition().Row) + 1,
					Column:    int(defNode.StartPosition().Column),
					EndLine:   int(defNode.EndPosition().Row) + 1,
					Signature: truncateSignature(defNode.Utf8Text(content)),
					Namespace: ns,
					Parent:    parent,
				})

			case "class.name":
				defNode := findEnclosingKind(nodePtr, "class_specifier")
				if defNode == nil {
					continue
				}
				ns := resolveNamespace(nodePtr, content)
				// For nested classes, find the outer class.
				parentClass := resolveParentClass(defNode, content)
				fg.Symbols = append(fg.Symbols, parser.Symbol{
					Name:      text,
					Kind:      parser.KindClass,
					File:      path,
					Line:      int(defNode.StartPosition().Row) + 1,
					Column:    int(defNode.StartPosition().Column),
					EndLine:   int(defNode.EndPosition().Row) + 1,
					Signature: truncateSignature(defNode.Utf8Text(content)),
					Namespace: ns,
					Parent:    parentClass,
				})

			case "struct.name":
				defNode := findEnclosingKind(nodePtr, "struct_specifier")
				if defNode == nil {
					continue
				}
				ns := resolveNamespace(nodePtr, content)
				parentClass := resolveParentClass(defNode, content)
				fg.Symbols = append(fg.Symbols, parser.Symbol{
					Name:      text,
					Kind:      parser.KindStruct,
					File:      path,
					Line:      int(defNode.StartPosition().Row) + 1,
					Column:    int(defNode.StartPosition().Column),
					EndLine:   int(defNode.EndPosition().Row) + 1,
					Signature: truncateSignature(defNode.Utf8Text(content)),
					Namespace: ns,
					Parent:    parentClass,
				})

			case "enum.name":
				defNode := findEnclosingKind(nodePtr, "enum_specifier")
				if defNode == nil {
					continue
				}
				ns := resolveNamespace(nodePtr, content)
				fg.Symbols = append(fg.Symbols, parser.Symbol{
					Name:      text,
					Kind:      parser.KindEnum,
					File:      path,
					Line:      int(defNode.StartPosition().Row) + 1,
					Column:    int(defNode.StartPosition().Column),
					EndLine:   int(defNode.EndPosition().Row) + 1,
					Signature: truncateSignature(defNode.Utf8Text(content)),
					Namespace: ns,
				})

			case "inline.name":
				// Inline method defined inside a class body (uses field_identifier).
				defNode := findEnclosingKind(nodePtr, "function_definition")
				if defNode == nil {
					continue
				}
				ns := resolveNamespace(nodePtr, content)
				parentClass := resolveParentClass(nodePtr, content)
				fg.Symbols = append(fg.Symbols, parser.Symbol{
					Name:      text,
					Kind:      parser.KindMethod,
					File:      path,
					Line:      int(defNode.StartPosition().Row) + 1,
					Column:    int(defNode.StartPosition().Column),
					EndLine:   int(defNode.EndPosition().Row) + 1,
					Signature: truncateSignature(defNode.Utf8Text(content)),
					Namespace: ns,
					Parent:    parentClass,
				})

			case "operator.name":
				defNode := findEnclosingKind(nodePtr, "function_definition")
				if defNode == nil {
					continue
				}
				ns := resolveNamespace(nodePtr, content)
				parentClass := resolveParentClass(nodePtr, content)
				fg.Symbols = append(fg.Symbols, parser.Symbol{
					Name:      text,
					Kind:      parser.KindFunction,
					File:      path,
					Line:      int(defNode.StartPosition().Row) + 1,
					Column:    int(defNode.StartPosition().Column),
					EndLine:   int(defNode.EndPosition().Row) + 1,
					Signature: truncateSignature(defNode.Utf8Text(content)),
					Namespace: ns,
					Parent:    parentClass,
				})

			case "typedef.name":
				// Handles simple typedefs, function pointer typedefs, and using aliases.
				defNode := findEnclosingKind(nodePtr, "type_definition")
				if defNode == nil {
					// Try alias_declaration for `using X = ...`
					defNode = findEnclosingKind(nodePtr, "alias_declaration")
				}
				if defNode == nil {
					continue
				}
				ns := resolveNamespace(nodePtr, content)
				fg.Symbols = append(fg.Symbols, parser.Symbol{
					Name:      text,
					Kind:      parser.KindTypedef,
					File:      path,
					Line:      int(defNode.StartPosition().Row) + 1,
					Column:    int(defNode.StartPosition().Column),
					EndLine:   int(defNode.EndPosition().Row) + 1,
					Signature: truncateSignature(defNode.Utf8Text(content)),
					Namespace: ns,
				})
			}
		}
	}
}

// extractCalls finds call expressions and produces call edges.
func (p *CppParser) extractCalls(root *tree_sitter.Node, content []byte, path string, fg *parser.FileGraph) {
	cursor := tree_sitter.NewQueryCursor()
	defer cursor.Close()

	matches := cursor.Matches(p.callQuery, root, content)
	for {
		match := matches.Next()
		if match == nil {
			break
		}

		for i := range match.Captures {
			capture := &match.Captures[i]
			node := capture.Node
			if node.HasError() {
				continue
			}

			capName := p.captureNameForIndex(p.callQuery, capture.Index)

			var calleeName string
			switch capName {
			case "call.name":
				calleeName = node.Utf8Text(content)
			case "call.qname":
				calleeName = node.Utf8Text(content)
			default:
				continue
			}

			// Filter out C++ cast expressions that tree-sitter parses as call_expression.
			if isCppCast(calleeName) {
				continue
			}

			// Find the enclosing call_expression for line info.
			nodePtr := &capture.Node
			callNode := findEnclosingKind(nodePtr, "call_expression")
			if callNode == nil {
				callNode = nodePtr
			}

			// Determine the enclosing function (From).
			from := enclosingFunctionID(nodePtr, content, path)

			fg.Edges = append(fg.Edges, parser.Edge{
				From: from,
				To:   calleeName,
				Kind: parser.EdgeCalls,
				File: path,
				Line: int(callNode.StartPosition().Row) + 1,
			})
		}
	}
}

// extractIncludes finds #include directives and produces include edges.
func (p *CppParser) extractIncludes(root *tree_sitter.Node, content []byte, path string, fg *parser.FileGraph) {
	cursor := tree_sitter.NewQueryCursor()
	defer cursor.Close()

	matches := cursor.Matches(p.inclQuery, root, content)
	for {
		match := matches.Next()
		if match == nil {
			break
		}

		for _, capture := range match.Captures {
			node := capture.Node
			if node.HasError() {
				continue
			}

			capName := p.captureNameForIndex(p.inclQuery, capture.Index)
			if capName != "include.path" {
				continue
			}

			raw := node.Utf8Text(content)
			cleaned := stripIncludePath(raw)

			fg.Edges = append(fg.Edges, parser.Edge{
				From: path,
				To:   cleaned,
				Kind: parser.EdgeIncludes,
				File: path,
				Line: int(node.StartPosition().Row) + 1,
			})
		}
	}
}

// extractInheritance finds base class specifiers and produces inherit edges.
func (p *CppParser) extractInheritance(root *tree_sitter.Node, content []byte, path string, fg *parser.FileGraph) {
	cursor := tree_sitter.NewQueryCursor()
	defer cursor.Close()

	matches := cursor.Matches(p.inhQuery, root, content)
	for {
		match := matches.Next()
		if match == nil {
			break
		}

		var derivedName string
		var baseNames []string

		for _, capture := range match.Captures {
			node := capture.Node
			if node.HasError() {
				continue
			}

			capName := p.captureNameForIndex(p.inhQuery, capture.Index)
			text := node.Utf8Text(content)

			switch capName {
			case "derived.name":
				derivedName = text
			case "base.name":
				baseNames = append(baseNames, text)
			}
		}

		for _, base := range baseNames {
			fg.Edges = append(fg.Edges, parser.Edge{
				From: derivedName,
				To:   base,
				Kind: parser.EdgeInherits,
				File: path,
				Line: 0, // line is approximate; could be refined
			})
		}
	}
}

// --- Helpers ---

// captureNameForIndex returns the capture name for a given capture index in a query.
func (p *CppParser) captureNameForIndex(query *tree_sitter.Query, index uint32) string {
	names := query.CaptureNames()
	if int(index) < len(names) {
		return names[index]
	}
	return ""
}

// findEnclosingKind walks up the parent chain to find a node of the given kind.
func findEnclosingKind(node *tree_sitter.Node, kind string) *tree_sitter.Node {
	for n := node; n != nil; n = n.Parent() {
		if n.Kind() == kind {
			return n
		}
	}
	return nil
}

// resolveNamespace walks up from node to find all enclosing namespace_definition
// ancestors and returns a joined namespace string like "a::b".
func resolveNamespace(node *tree_sitter.Node, content []byte) string {
	var parts []string
	for n := node.Parent(); n != nil; n = n.Parent() {
		if n.Kind() == "namespace_definition" {
			nameNode := n.ChildByFieldName("name")
			if nameNode != nil {
				parts = append(parts, nameNode.Utf8Text(content))
			}
			// Anonymous namespace — no name child, skip.
		}
	}
	// Reverse: outermost namespace first.
	for i, j := 0, len(parts)-1; i < j; i, j = i+1, j-1 {
		parts[i], parts[j] = parts[j], parts[i]
	}
	return strings.Join(parts, "::")
}

// resolveParentClass walks up the AST to find an enclosing class_specifier or
// struct_specifier and returns its name. Used for inline methods and operators
// defined inside class bodies.
func resolveParentClass(node *tree_sitter.Node, content []byte) string {
	for n := node.Parent(); n != nil; n = n.Parent() {
		if n.Kind() == "class_specifier" || n.Kind() == "struct_specifier" {
			nameNode := n.ChildByFieldName("name")
			if nameNode != nil {
				return nameNode.Utf8Text(content)
			}
		}
	}
	return ""
}

// enclosingFunctionID finds the enclosing function_definition and returns a
// symbol ID like "path:funcName" or "path:Class::method". Falls back to path
// if no enclosing function is found (top-level call).
func enclosingFunctionID(node *tree_sitter.Node, content []byte, path string) string {
	funcDef := findEnclosingKind(node, "function_definition")
	if funcDef == nil {
		return path
	}

	// Extract the function name from the declarator.
	declarator := funcDef.ChildByFieldName("declarator")
	if declarator == nil {
		return path
	}

	// The declarator is a function_declarator; its own declarator child is the name.
	if declarator.Kind() == "function_declarator" {
		nameNode := declarator.ChildByFieldName("declarator")
		if nameNode != nil {
			return path + ":" + nameNode.Utf8Text(content)
		}
	}

	return path
}

// isCppCast returns true if the name is a C++ cast keyword that tree-sitter
// parses as a call_expression.
func isCppCast(name string) bool {
	switch name {
	case "static_cast", "dynamic_cast", "const_cast", "reinterpret_cast":
		return true
	}
	return false
}

// splitQualified splits "Scope::Name" into (scope, name). If there is no "::",
// returns ("", original).
func splitQualified(qualified string) (string, string) {
	idx := strings.LastIndex(qualified, "::")
	if idx < 0 {
		return "", qualified
	}
	return qualified[:idx], qualified[idx+2:]
}

// stripIncludePath removes surrounding quotes or angle brackets from an include path.
func stripIncludePath(raw string) string {
	if len(raw) >= 2 {
		if (raw[0] == '"' && raw[len(raw)-1] == '"') ||
			(raw[0] == '<' && raw[len(raw)-1] == '>') {
			return raw[1 : len(raw)-1]
		}
	}
	return raw
}

// truncateSignature truncates a signature at the first `{` or `;`, keeping
// only the declaration line. Falls back to a byte limit if neither is found.
func truncateSignature(s string) string {
	for i, c := range s {
		if c == '{' || c == ';' {
			return strings.TrimRight(s[:i], " \t\n\r")
		}
		if i >= 200 {
			return s[:i] + "..."
		}
	}
	return s
}
