// Method-reference call shapes. The identifier-on-RHS forms
// (`String::length`, `obj::method`, `this::doIt`, `super::doIt`) are
// recorded as Calls edges with `to = <RHS>` (Phase 3.3). The
// constructor-reference form `Type::new` is the documented limitation
// — the query does NOT match it (no Calls edge produced).

package edge_cases;

import java.util.function.Function;
import java.util.function.Supplier;

public class MethodReferences {

    public static int len(String s) {
        return s.length();
    }

    public void run() {
        Function<String, Integer> a = String::length;
        Function<String, Integer> b = this::len;

        // Constructor reference — documented limitation, no Calls edge.
        Supplier<MethodReferences> c = MethodReferences::new;

        a.apply("hi");
        b.apply("there");
        c.get();
    }
}
