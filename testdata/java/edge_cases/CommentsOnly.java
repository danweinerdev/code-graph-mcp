// Only comments around an otherwise-empty class shell. Java requires
// at least one top-level class for the file to parse, so the file
// produces 1 Class symbol (CommentsOnly) and 0 method symbols.

package edge_cases;

/* Multi-line block comment
   spanning multiple lines.
   No methods or fields should be extracted. */

/**
 * Javadoc comment with no following member.
 */
public class CommentsOnly {
    // Only comments inside the body, no declarations.

    /* Inner block comment. */

    /** Inner javadoc. */
}
