"""Matchers: predicates and descriptions."""

from __future__ import annotations

import pytest
from frequenz.quantities import Percentage, Power

from macrocosim._matchers import at_least, at_most, between, near

pytestmark = pytest.mark.filterwarnings("error")

W = Power.from_watts
kW = Power.from_kilowatts


def test_approx_max_min_shorthands() -> None:
    assert near(kW(1), tol=W(50)).matches(kW(1))
    assert at_most(kW(2)).matches(kW(1))
    assert at_least(W(0)).matches(kW(1))
    assert not at_most(W(500)).matches(kW(1))


def test_near_is_inclusive_at_the_tolerance() -> None:
    m = near(W(100), tol=W(10))
    assert m.matches(W(90)) and m.matches(W(110))
    assert not m.matches(W(111)) and not m.matches(W(89))


def test_between_is_closed() -> None:
    m = between(W(1900), W(2100))
    assert m.matches(W(1900)) and m.matches(W(2100)) and m.matches(W(2000))
    assert not m.matches(W(2101))


def test_descriptions() -> None:
    assert near(W(0), tol=kW(1)).describe() == "≈ 0 W ± 1 kW"
    assert between(W(1), W(2)).describe() == "within [1 W, 2 W]"
    assert at_most(kW(15)).describe() == "<= 15 kW"
    assert at_least(W(0)).describe() == ">= 0 W"


def test_bounds_are_inclusive() -> None:
    assert at_most(W(5)).matches(W(5))
    assert at_least(W(5)).matches(W(5))
    assert between(W(1), W(5)).matches(W(1))
    assert not between(W(1), W(5)).matches(W(0.5))


def test_bad_matcher_arguments_raise_before_any_io() -> None:
    with pytest.raises(ValueError, match="tol must not be negative"):
        near(W(5), tol=W(-1))
    with pytest.raises(ValueError, match="low must not be above high"):
        between(W(5), W(1))
    with pytest.raises(TypeError, match="tol must be Power"):
        near(W(5), tol=Percentage.from_percent(1))  # type: ignore[arg-type]
    with pytest.raises(TypeError, match="high must be Power"):
        between(W(1), Percentage.from_percent(5))  # type: ignore[arg-type]
    nan = W(float("nan"))
    with pytest.raises(ValueError, match="value must be finite"):
        near(nan, tol=W(1))
    with pytest.raises(ValueError, match="tol must be finite"):
        near(W(5), tol=nan)
    with pytest.raises(ValueError, match="low must be finite"):
        between(nan, W(1))
    with pytest.raises(ValueError, match="high must be finite"):
        between(W(1), nan)
    with pytest.raises(ValueError, match="ceiling must be finite"):
        at_most(nan)
    with pytest.raises(ValueError, match="floor must be finite"):
        at_least(nan)
