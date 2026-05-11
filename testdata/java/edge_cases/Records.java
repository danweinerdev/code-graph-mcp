// Decision 6: `record User(String name)` extracts as Class — NOT as a
// new SymbolKind::Record. Methods inside the record body extract as
// Method with parent = `User` (NOT as orphan Function symbols — the
// records-leak bug C# 2.2 fixed in `0cf200b` is mirrored here).

package edge_cases;

public record Records(String name, int age) {

    public String greeting() {
        return "Hello, " + name;
    }

    public int nextAge() {
        return age + 1;
    }
}
