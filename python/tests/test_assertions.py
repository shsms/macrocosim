"""Assertion-layer tests — pure, using a fake read (no binary)."""

from __future__ import annotations

from datetime import timedelta

import pytest
from frequenz.quantities import Power

from switchyard.assertions import Assertion

_FAST = {"timeout": timedelta(seconds=0.2), "poll": timedelta(seconds=0.02)}


def _w(watts: float) -> Power:
    return Power.from_watts(watts)


async def test_eventually_passes_when_predicate_holds() -> None:
    a = Assertion(lambda: _w(2000), "x")
    got = await a.eventually(within=(_w(1900), _w(2100)), timeout=timedelta(seconds=1))
    assert got == _w(2000)


async def test_eventually_raises_when_never_holds() -> None:
    a = Assertion(lambda: _w(500), "x")
    with pytest.raises(AssertionError, match="≈"):
        await a.eventually(approx=_w(2000), tol=_w(100), **_FAST)


async def test_approx_max_min_shorthands() -> None:
    a = Assertion(lambda: _w(1000), "x")
    assert await a.approx(_w(1000), tol=_w(50)) == _w(1000)
    assert await a.max(Power.from_kilowatts(2)) == _w(1000)
    assert await a.min(_w(0)) == _w(1000)
    with pytest.raises(AssertionError):
        await a.max(_w(500), **_FAST)


async def test_none_reads_never_satisfy() -> None:
    a: Assertion[Power] = Assertion(lambda: None, "x")
    with pytest.raises(AssertionError):
        await a.eventually(min=_w(0), **_FAST)


async def test_always_holds_then_breaks() -> None:
    steady = Assertion(lambda: _w(100), "x")
    series = await steady.always(
        max=_w(200), for_=timedelta(seconds=0.15), poll=timedelta(seconds=0.02)
    )
    assert series and all(v == _w(100) for v in series)

    reads = iter([_w(100), _w(100), _w(999)])
    spiking = Assertion(lambda: next(reads, _w(999)), "x")
    with pytest.raises(AssertionError, match="broke at"):
        await spiking.always(
            max=_w(200), for_=timedelta(seconds=1), poll=timedelta(seconds=0.01)
        )


async def test_always_accepts_approx_and_tol() -> None:
    # "hold approximately X for a duration" — approx/tol work with for_.
    steady = Assertion(lambda: _w(100), "x")
    series = await steady.always(
        approx=_w(100),
        tol=_w(10),
        for_=timedelta(seconds=0.1),
        poll=timedelta(seconds=0.02),
    )
    assert series and all(v == _w(100) for v in series)

    drifting = Assertion(lambda: _w(500), "x")
    with pytest.raises(AssertionError, match="broke at"):
        await drifting.always(
            approx=_w(100),
            tol=_w(10),
            for_=timedelta(seconds=1),
            poll=timedelta(seconds=0.01),
        )


async def test_always_skips_none_reads() -> None:
    # warm-up / transient None reads must not be treated as a breach.
    reads = iter([None, _w(100), None, _w(100)])
    a = Assertion(lambda: next(reads, _w(100)), "x")
    series = await a.always(
        max=_w(200), for_=timedelta(seconds=0.1), poll=timedelta(seconds=0.01)
    )
    assert series and all(v is not None for v in series)


async def test_once_checks_a_single_available_reading() -> None:
    # First read is already available and in range → returns it, and does
    # NOT keep polling (a later 999 would break `max`, but `once` is done).
    reads = iter([_w(50), _w(999)])
    a = Assertion(lambda: next(reads, _w(999)), "x")
    assert await a.once(max=_w(100)) == _w(50)


async def test_once_waits_for_the_first_non_none_reading() -> None:
    # The stream hasn't published yet on the first couple of reads; `once`
    # awaits availability rather than failing on the transient None.
    reads = iter([None, None, _w(42)])
    a = Assertion(lambda: next(reads, None), "x")
    assert await a.once(max=_w(100), **_FAST) == _w(42)


async def test_once_raises_on_persistent_none() -> None:
    a: Assertion[Power] = Assertion(lambda: None, "x")
    with pytest.raises(AssertionError, match="measured None"):
        await a.once(max=_w(100), **_FAST)


async def test_once_raises_when_out_of_range() -> None:
    a = Assertion(lambda: _w(150), "x")
    with pytest.raises(AssertionError, match="in \\["):
        await a.once(max=_w(100), **_FAST)


async def test_approx_without_tol_raises() -> None:
    a = Assertion(lambda: _w(100), "x")
    with pytest.raises(ValueError, match="tol"):
        await a.eventually(approx=_w(100))


async def test_no_matcher_raises() -> None:
    a: Assertion[Power] = Assertion(lambda: _w(100), "x")
    with pytest.raises(ValueError, match="no matcher"):
        await a.eventually()
