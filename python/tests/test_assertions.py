"""Assertion-layer tests — pure, using a fake read (no binary)."""

from __future__ import annotations

from datetime import timedelta

import pytest
from frequenz.quantities import Power

from switchyard.assertions import Assertion

_FAST = {"timeout": timedelta(seconds=0.2), "poll": timedelta(seconds=0.02)}


def _w(watts: float) -> Power:
    return Power.from_watts(watts)


def test_eventually_passes_when_predicate_holds() -> None:
    a = Assertion(lambda: _w(2000), "x")
    got = a.eventually(within=(_w(1900), _w(2100)), timeout=timedelta(seconds=1))
    assert got == _w(2000)


def test_eventually_raises_when_never_holds() -> None:
    a = Assertion(lambda: _w(500), "x")
    with pytest.raises(AssertionError, match="≈"):
        a.eventually(approx=_w(2000), tol=_w(100), **_FAST)


def test_approx_max_min_shorthands() -> None:
    a = Assertion(lambda: _w(1000), "x")
    assert a.approx(_w(1000), tol=_w(50)) == _w(1000)
    assert a.max(Power.from_kilowatts(2)) == _w(1000)
    assert a.min(_w(0)) == _w(1000)
    with pytest.raises(AssertionError):
        a.max(_w(500), **_FAST)


def test_none_reads_never_satisfy() -> None:
    a: Assertion[Power] = Assertion(lambda: None, "x")
    with pytest.raises(AssertionError):
        a.eventually(min=_w(0), **_FAST)


def test_always_holds_then_breaks() -> None:
    steady = Assertion(lambda: _w(100), "x")
    series = steady.always(
        max=_w(200), for_=timedelta(seconds=0.15), poll=timedelta(seconds=0.02)
    )
    assert series and all(v == _w(100) for v in series)

    reads = iter([_w(100), _w(100), _w(999)])
    spiking = Assertion(lambda: next(reads, _w(999)), "x")
    with pytest.raises(AssertionError, match="broke at"):
        spiking.always(
            max=_w(200), for_=timedelta(seconds=1), poll=timedelta(seconds=0.01)
        )


def test_always_skips_none_reads() -> None:
    # warm-up / transient None reads must not be treated as a breach.
    reads = iter([None, _w(100), None, _w(100)])
    a = Assertion(lambda: next(reads, _w(100)), "x")
    series = a.always(
        max=_w(200), for_=timedelta(seconds=0.1), poll=timedelta(seconds=0.01)
    )
    assert series and all(v is not None for v in series)


def test_approx_without_tol_raises() -> None:
    a = Assertion(lambda: _w(100), "x")
    with pytest.raises(ValueError, match="tol"):
        a.eventually(approx=_w(100))


def test_no_matcher_raises() -> None:
    a: Assertion[Power] = Assertion(lambda: _w(100), "x")
    with pytest.raises(ValueError, match="no matcher"):
        a.eventually()
