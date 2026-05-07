# Deeply nested classes: parent IDs concatenate to the innermost
# enclosing class only (the parser records bare `Outer.Inner.Deep.Deepest`
# style is NOT used — only the immediate parent class name).

class Outer:
    class Mid:
        class Inner:
            class Deepest:
                def leaf(self):
                    return 1
