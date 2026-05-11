// Decision 3: `partial class Foo` declarations across multiple files
// produce TWO Class symbols both named `Foo`, disambiguated by file path.
// This is half of the cross-file partial-class fixture; the other half
// lives in `partial_class_b.cs`.

namespace Partials
{
    public partial class Foo
    {
        public void A() { }
    }
}
