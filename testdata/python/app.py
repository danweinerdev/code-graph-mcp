"""Application entry point.

Imports across the package and uses the public surface — covers
`from __future__ import annotations`, `from . import ...`, and
attribute-style + direct calls into other modules.
"""

from __future__ import annotations

from . import utils
from .handlers import handle
from .models import Alpha


def run() -> None:
    """Top-level entry point used by tests and the corpus walker."""
    a = Alpha("hello")
    handle(a)
    utils.kw(1, 2, key="value")


def main() -> None:
    run()
