// 2-level nested class: Inner records the *immediate* enclosing outer
// class as parent (bare name `Outer`), NOT a dotted path. Leaf method
// records `Inner` for the same reason.

package edge_cases;

public class NestedClasses {
    public static class Outer {
        public static class Inner {
            public void leaf() { }
        }
    }
}
