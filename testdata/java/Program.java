// Entry point exercising plain imports, wildcard imports, static imports,
// constructor calls, and chained method invocations.

package app;

import java.util.ArrayList;
import java.util.List;
import java.util.*;
import static java.lang.Math.abs;

public class Program {

    public static void main(String[] args) {
        List<String> items = new ArrayList<>();
        items.add("hello");
        items.size();
        run();
        abs(-1);
    }

    public static void run() {
        System.out.println("running");
    }
}
