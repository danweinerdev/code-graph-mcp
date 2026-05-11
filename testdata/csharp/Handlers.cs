// Default interface methods (Decision 11), records (Decision 6 analog),
// extension methods (Decision 5), using static + alias + global.

using static System.Math;
using StrList = System.Collections.Generic.List<string>;
global using System.Linq;

namespace Handlers
{
    public interface IGreeter
    {
        // Abstract — produces NO symbol (forward-declaration rule).
        void Required();

        // Default interface method (C# 8+) — extracts as Function per
        // Decision 11's C# follow-up.
        void Greet() { Required(); }
    }

    public record User(string Name)
    {
        public string Display() { return Name; }
    }

    public static class StringExt
    {
        // Extension method (Decision 5): parent is the syntactic
        // enclosing static class `StringExt`, NOT `string`.
        public static int CountWords(this string s)
        {
            return Abs(s.Length);
        }
    }

    public class Hub
    {
        public void Dispatch(StrList items)
        {
            foreach (var item in items)
            {
                Process(item);
            }
        }

        public static void Process(string item) { }
    }
}
