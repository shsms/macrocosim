"""Matchers: what an `expect` call compares each sample against.

Internal; imports `_units`. Build one with `near`, `between`, `at_most` or
`at_least`. A matcher only ever sees quantities, never the floats the simulator
sends.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any, Generic, Protocol, TypeVar

from ._units import check_finite, check_type

Q = TypeVar("Q")
Q_contra = TypeVar("Q_contra", contravariant=True)


class Matcher(Protocol[Q_contra]):
    """A predicate on one sample, with a description for failure messages."""

    def matches(self, value: Q_contra) -> bool:
        """Whether `value` satisfies the matcher."""
        ...

    def describe(self) -> str:
        """The matcher as it appears in a message, for example ``<= 15 kW``."""
        ...


@dataclass(frozen=True)
class _Near(Generic[Q]):
    value: Q
    tol: Q

    def matches(self, value: Q) -> bool:
        v: Any = value
        return abs(v - self.value) <= self.tol

    def describe(self) -> str:
        return f"≈ {self.value} ± {self.tol}"


@dataclass(frozen=True)
class _Between(Generic[Q]):
    low: Q
    high: Q

    def matches(self, value: Q) -> bool:
        v: Any = value
        return self.low <= v <= self.high

    def describe(self) -> str:
        return f"within [{self.low}, {self.high}]"


@dataclass(frozen=True)
class _AtMost(Generic[Q]):
    ceiling: Q

    def matches(self, value: Q) -> bool:
        v: Any = value
        return v <= self.ceiling

    def describe(self) -> str:
        return f"<= {self.ceiling}"


@dataclass(frozen=True)
class _AtLeast(Generic[Q]):
    floor: Q

    def matches(self, value: Q) -> bool:
        v: Any = value
        return v >= self.floor

    def describe(self) -> str:
        return f">= {self.floor}"


def near(value: Q, *, tol: Q) -> Matcher[Q]:
    """Match values equal to `value` within `tol`, the edge included.

    `tol` must have the type of `value` and must not be negative; both must be
    finite.
    """
    check_type("tol", tol, type(value))
    check_finite("value", value)
    check_finite("tol", tol)
    t: Any = tol
    zero = t - t  # the zero of tol's type
    if t < zero:
        raise ValueError(f"tol must not be negative, not {tol}")
    return _Near(value, tol)


def between(low: Q, high: Q) -> Matcher[Q]:
    """Match values inside the closed interval [`low`, `high`].

    Both must have the same type and be finite, and `low` must not be above
    `high`.
    """
    check_type("high", high, type(low))
    check_finite("low", low)
    check_finite("high", high)
    lo: Any = low
    if lo > high:
        raise ValueError(f"low must not be above high: {low} > {high}")
    return _Between(low, high)


def at_most(ceiling: Q) -> Matcher[Q]:
    """Match values at or below `ceiling`, which must be finite."""
    check_finite("ceiling", ceiling)
    return _AtMost(ceiling)


def at_least(floor: Q) -> Matcher[Q]:
    """Match values at or above `floor`, which must be finite."""
    check_finite("floor", floor)
    return _AtLeast(floor)
