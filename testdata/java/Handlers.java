// Decision 11 default-interface-method (extracted as Function), Decision 6
// records-as-Class, sealed interface (permits ignored), static-import.

package handlers;

import static java.lang.Math.max;

public class Handlers {

    public interface IGreeter {
        // Abstract — produces NO symbol (forward-declaration rule).
        void required();

        // Default interface method — Decision 11 extracts as Function
        // (no parent), matching Rust's trait-default-method rule.
        default void greet() {
            required();
        }

        // Static interface method (Java 8+) — also extracts as Function.
        static String banner() {
            return "hi";
        }
    }

    // Sealed interface — Decision 6: `permits` clause is ignored and
    // extracts as ordinary Interface.
    public sealed interface Shape permits Circle, Square { }

    public static final class Circle implements Shape { }
    public static final class Square implements Shape { }

    // record User — Decision 6: extracts as Class. Method inside the
    // record body extracts as Method with parent = `User`.
    public record User(String name) {
        public String display() {
            return name;
        }
    }

    public static class Hub {
        public void dispatch(java.util.List<String> items) {
            for (String item : items) {
                process(item);
            }
            max(1, 2);
        }

        public static void process(String item) { }
    }
}
