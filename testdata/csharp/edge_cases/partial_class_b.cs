// Decision 3: companion to `partial_class_a.cs`. Together the two
// files declare `partial class Foo` twice; the parser produces a Class
// symbol from each declaration.

namespace Partials
{
    public partial class Foo
    {
        public void B() { }
    }
}
