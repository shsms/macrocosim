"""Typed assertion matchers — one matcher per expect, by construction.

Instead of a bag of optional keywords (where "``approx`` without ``tol``"
and "no matcher given" are runtime errors), an expect takes exactly one
:class:`Matcher`, built by a constructor that demands its parts::

    await site.grid_power.expect(at_most(kW(13)))
    await site.grid_power.expect(near(kW(20), tol=kW(1)))
    await bat.soc.expect(between(percent(45), percent(55)))

A ``Matcher[Q]`` is generic over the quantity, so an Energy bound against
a Power signal is a type error, not a comparison failure mid-poll.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Generic, TypeVar

from frequenz.quantities import Quantity

Q = TypeVar("Q", bound=Quantity)


@dataclass(frozen=True)
class Matcher(Generic[Q]):
    """A value predicate for one ``expect`` call.

    Build one with :func:`near`, :func:`between`, :func:`at_most`, or
    :func:`at_least` — not directly. Exactly one of the field groups is
    set, matching the assertion engine's matcher keywords.
    """

    description: str
    """Human phrasing, for failure messages and reprs."""

    approx: Q | None = None
    """Match values equal to this, within :attr:`tol`."""

    tol: Q | None = None
    """The tolerance for :attr:`approx`."""

    within: tuple[Q, Q] | None = None
    """Match values inside this closed interval."""

    max: Q | None = None
    """Match values at or below this."""

    min: Q | None = None
    """Match values at or above this."""


def near(value: Q, *, tol: Q) -> Matcher[Q]:
    """Match values equal to ``value`` within ``tol`` (both required)."""
    return Matcher(description=f"≈ {value} ± {tol}", approx=value, tol=tol)


def between(low: Q, high: Q) -> Matcher[Q]:
    """Match values inside the closed interval [``low``, ``high``]."""
    return Matcher(description=f"within [{low}, {high}]", within=(low, high))


def at_most(ceiling: Q) -> Matcher[Q]:
    """Match values at or below ``ceiling``."""
    return Matcher(description=f"<= {ceiling}", max=ceiling)


def at_least(floor: Q) -> Matcher[Q]:
    """Match values at or above ``floor``."""
    return Matcher(description=f">= {floor}", min=floor)
