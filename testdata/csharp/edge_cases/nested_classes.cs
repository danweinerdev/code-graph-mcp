// 2-level nested class: Inner records the *immediate* enclosing
// outer class as parent (bare name `Outer`), NOT a dotted path.

namespace Nested
{
    public class Outer
    {
        public class Inner
        {
            public void Leaf() { }
        }
    }
}
