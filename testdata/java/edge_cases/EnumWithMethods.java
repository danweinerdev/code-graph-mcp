// Decision 12: the Planet fixture. enum-level methods AND per-constant
// methods both extract as Method with parent = enum type (`Planet`) —
// NOT `Planet$EARTH`. Enum constants themselves (EARTH, MARS) are NOT
// extracted as symbols. Enum-level abstract methods (no body) are
// filtered as forward declarations.

package edge_cases;

public enum EnumWithMethods {
    EARTH {
        @Override
        public double surfaceGravity() {
            return 9.8;
        }
    },
    MARS {
        @Override
        public double surfaceGravity() {
            return 3.71;
        }
    };

    // Enum-level abstract method — no body, dropped per the
    // forward-declaration rule (zero symbols emitted).
    public abstract double surfaceGravity();

    // Enum-level concrete method — body present, extracts as Method
    // with parent = `EnumWithMethods`.
    public String describe() {
        return name() + " has gravity " + surfaceGravity();
    }
}
