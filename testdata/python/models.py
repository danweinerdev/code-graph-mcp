"""Model classes — covers single, multiple, and qualified inheritance,
ABC + @abstractmethod, and the dataclass-style __slots__ pattern.

The mix is deliberate: each class form maps to a specific row in
MANIFEST.md so a regression in inheritance extraction localizes to one
fixture line.
"""

import abc


class Mixin:
    def mixed(self):
        return "mixed"


class Alpha:
    def __init__(self, label):
        self.label = label

    def __str__(self):
        return self.label

    def __repr__(self):
        return "Alpha(" + repr(self.label) + ")"


class Beta(Alpha):
    def __init__(self, label, count):
        super().__init__(label)
        self.count = count


class Gamma(Alpha, Mixin):
    """Multiple inheritance: Alpha provides label/__str__, Mixin provides mixed."""

    def combine(self):
        return self.mixed() + ":" + str(self)


class Delta(abc.ABC):
    """Qualified base — `to` field records `abc.ABC` verbatim."""

    @abc.abstractmethod
    def required(self):
        pass


class WithSlots:
    """Dataclass-style fixed-field class. __slots__ is a class-level
    assignment — no inheritance edge."""

    __slots__ = ("x",)

    def __init__(self, x):
        self.x = x
