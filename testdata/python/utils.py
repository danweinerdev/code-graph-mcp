"""Free functions: type alias, generator, and *args/**kwargs signature.

The type alias is a module-level assignment — no symbol falls out of
the parser; only `def` and `class` produce symbols.
"""

from typing import Dict

Result = Dict[str, int]


def gen():
    """Generator — extracted as a Function (no special generator kind)."""
    yield 1
    yield 2


def kw(*args, **kwargs):
    """Variadic signature — *args/**kwargs preserved in the captured
    signature text via truncate_signature."""
    return (args, kwargs)


def add(a, b):
    return a + b
