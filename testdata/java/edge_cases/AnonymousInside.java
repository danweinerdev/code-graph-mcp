// Decision 4: anonymous classes emit NO Class symbol. Methods inside the
// anonymous body take the ENCLOSING NAMED ENTITY's parent — `AnonymousInside`.
// Two anonymous classes inside the same enclosing method that both define
// `run()` produce two `AnonymousInside::run` symbols disambiguated only
// by `Symbol.line`. This is the load-bearing Decision 4 anti-regression
// fixture.

package edge_cases;

public class AnonymousInside {

    public void handle() {
        Runnable first = new Runnable() {
            @Override
            public void run() {
                System.out.println("first");
            }
        };

        Runnable second = new Runnable() {
            @Override
            public void run() {
                System.out.println("second");
            }
        };

        first.run();
        second.run();
    }
}
