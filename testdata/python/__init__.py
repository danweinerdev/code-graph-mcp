"""Package init exposing the public surface for the corpus fixture.

Re-exports Alpha + Beta from `models` and `handle` from `handlers` so a
downstream `from testdata_python import Alpha` is a one-liner. The
`__all__` controls `from pkg import *` and is independent of the parser
output (it produces no graph edges of its own).
"""

from .models import Alpha, Beta
from .handlers import handle

__all__ = ["Alpha", "Beta", "handle"]
