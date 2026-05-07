"""Decorated functions, async free + method, and a closure-returning factory.

Decorators are transparent for definition extraction (the parser sees
the inner `function_definition`); the corpus test pins this by
asserting `Service::value` (a @property) is a Method, not a Function.
"""

import asyncio


class Service:
    def __init__(self, name):
        self.name = name

    @property
    def value(self):
        return self.name

    @staticmethod
    def factory():
        return Service("default")

    @classmethod
    def from_name(cls, name):
        return cls(name)

    async def handle(self, payload):
        await asyncio.sleep(0)
        return payload


async def fetch():
    """Async free function — extracted as Function, no special async kind."""
    await asyncio.sleep(0)
    return None


def make_handler():
    """Closure factory — `inner` is a nested function that becomes a
    Function symbol with no class parent (Python nested functions have
    no enclosing class, only an enclosing function which the parser
    does not record as parent)."""

    def inner():
        return "inner"

    return inner


def handle(item):
    """Module-level entry point referenced by `__init__.__all__`."""
    svc = Service.factory()
    return svc.value
