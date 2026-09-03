"""The metric model and the kind-aware assertion engine."""

from __future__ import annotations

from datetime import timedelta

import pytest
from frequenz.quantities import Energy, Power

from macrocosim.assertions import expect_metric
from macrocosim.metrics import GRID_ENERGY, GRID_POWER, MetricKind, MetricSpec

_FAST = {"timeout": timedelta(seconds=0.2), "poll": timedelta(seconds=0.01)}


def _w(value: float) -> Power:
    return Power.from_watts(value)


def _wh(value: float) -> Energy:
    return Energy.from_watt_hours(value)


def test_bind_defaults_the_label_to_the_metric_name() -> None:
    bound = GRID_POWER.bind(lambda: _w(1))
    assert bound.label == "grid_power"
    assert bound.spec.kind is MetricKind.INSTANTANEOUS

    labelled = GRID_POWER.bind(lambda: _w(1), label="site 2 grid_power")
    assert labelled.label == "site 2 grid_power"


async def test_instantaneous_metric_settles() -> None:
    # The value converges after a few reads; the engine polls until the
    # matcher holds (eventually semantics).
    reads = iter([_w(900), _w(900), _w(100)])
    bound = GRID_POWER.bind(lambda: next(reads, _w(100)))
    value = await expect_metric(bound, max=_w(200), **_FAST)
    assert value == _w(100)


async def test_instantaneous_metric_with_for_holds_every_sample() -> None:
    steady = GRID_POWER.bind(lambda: _w(100))
    series = await expect_metric(
        steady, max=_w(200), for_=timedelta(seconds=0.1), poll=timedelta(seconds=0.02)
    )
    assert isinstance(series, list)
    assert series and all(v == _w(100) for v in series)

    reads = iter([_w(100), _w(999)])
    spiking = GRID_POWER.bind(lambda: next(reads, _w(999)))
    with pytest.raises(AssertionError, match="broke at"):
        await expect_metric(
            spiking,
            max=_w(200),
            for_=timedelta(seconds=1),
            poll=timedelta(seconds=0.01),
        )


async def test_cumulative_metric_is_checked_once() -> None:
    # A monotonic total: polling a max would pass early and break later.
    # The engine must check the matcher a single time — on the first
    # available value — so the growing total does not turn a pass into a
    # flake (nor a fail into a pass).
    reads = iter([None, _wh(50), _wh(500), _wh(5000)])
    bound = GRID_ENERGY.bind(lambda: next(reads, _wh(5000)))
    value = await expect_metric(bound, max=_wh(100), **_FAST)
    # The None was awaited past; the first real value was the one checked.
    assert value == _wh(50)


async def test_cumulative_metric_rejects_for_() -> None:
    bound = GRID_ENERGY.bind(lambda: _wh(1))
    with pytest.raises(ValueError, match="for_"):
        await expect_metric(bound, max=_wh(100), for_=timedelta(seconds=1))


async def test_engine_failures_carry_the_label() -> None:
    bound = GRID_POWER.bind(lambda: _w(999), label="site 7 grid_power")
    with pytest.raises(AssertionError, match="site 7 grid_power"):
        await expect_metric(bound, max=_w(1), **_FAST)


def test_specs_are_data() -> None:
    # Adding a metric is one spec entry, not new methods: a downstream
    # suite can describe and bind its own.
    custom = MetricSpec("water_flow", MetricKind.INSTANTANEOUS, Power)
    assert custom.bind(lambda: None).label == "water_flow"
