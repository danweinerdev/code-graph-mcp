# Method named `add` and a free function named `add` in the same module.
# The two symbols must coexist without collision.

class Adder:
    def add(self, a, b):
        return a + b


def add(a, b):
    return a + b
