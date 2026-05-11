// A method `Foo` and a free local function `Foo` coexist in the same
// file as distinct symbols. C# doesn't have module-level free functions
// like Python's `def add():`, but `static class` + static method is
// the idiomatic equivalent — the static method `Foo` on the static
// class is the "free function" half, and the instance method `Foo` on
// `Container` is the "method" half. Both produce SymbolKind::Method
// (C# methods are always class-bound), but the parent strings differ
// (`Container` vs `FreeFunctions`), so the symbol IDs are distinct.

namespace Collide
{
    public class Container
    {
        public void Foo() { }
    }

    public static class FreeFunctions
    {
        public static void Foo() { }
    }
}
