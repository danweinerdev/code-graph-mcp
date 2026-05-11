// Intentionally malformed: tree-sitter parses this with ERROR nodes.
// The parser must skip error nodes gracefully without panic. The
// recovered symbol count is what tree-sitter-java 0.23.5 actually
// produces — run and record, not zero.

package edge_cases;

public class Broken {
    public void bar(
    {
    }

    public void good() { }
}

class AlsoGood {
    public void run() { }
}
