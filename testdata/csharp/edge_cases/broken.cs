// Intentionally malformed: tree-sitter parses this with ERROR nodes.
// The parser must skip error nodes gracefully without panic.

namespace Bad
{
    public class Foo
    {
        public void Bar(
        {
        }

        public void Good() { }
    }

    public class AlsoGood
    {
        public void Run() { }
    }
}
