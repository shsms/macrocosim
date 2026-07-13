"""The async core: routing, guards, and the generic expect surface.

No server needed — transports are monkeypatched. The live end-to-end
pass is in ``test_aio_live.py`` (skips without a simulator binary).
"""

from __future__ import annotations

from datetime import timedelta
from typing import Any

import pytest
from frequenz.quantities import Energy, Power

import switchyard as sw
from switchyard.errors import EvalRejected
from switchyard.metrics import ACTIVE_POWER, GRID_ENERGY, GRID_POWER

_FAST = {"timeout": timedelta(seconds=0.2), "poll": timedelta(seconds=0.01)}


def _site(**latest: Any) -> sw.aio.Site:
    site = sw.aio.connect(
        ui="127.0.0.1:9",
        microgrids={1: sw.MicrogridEndpoint(id=1, name="a", grpc="10.0.0.1:61000")},
    )

    async def fake_latest(mg_id: int | None = None) -> dict[str, Any]:
        return {name: {"value": value} for name, value in latest.items()}

    site.latest = fake_latest  # type: ignore[method-assign]
    return site


def test_no_microgrids_raises_a_clear_error() -> None:
    site = sw.aio.connect(ui="127.0.0.1:9")
    with pytest.raises(RuntimeError, match="no microgrid endpoints"):
        _ = site.grpc


def test_grpc_url_prefixes_scheme() -> None:
    site = _site()
    assert site.grpc_url == "grpc://10.0.0.1:61000"


async def test_reads_map_metric_names_to_streams() -> None:
    # battery_* metrics read the battery_pool_* streams; the mapping lives
    # in one table, not in each method.
    site = _site(battery_pool_power=-2000.0, battery_pool_energy=-50.0)
    assert await site.battery_power() == Power.from_watts(-2000.0)
    assert await site.battery_energy() == Energy.from_watt_hours(-50.0)
    assert await site.grid_power() is None  # stream absent -> None


async def test_generic_expect_settles_on_an_instantaneous_metric() -> None:
    site = _site(grid_power=100.0)
    value = await site.expect(GRID_POWER, max=Power.from_watts(200), **_FAST)
    assert value == Power.from_watts(100.0)


async def test_generic_expect_checks_a_cumulative_metric_once() -> None:
    site = _site(grid_energy=50.0)
    value = await site.expect(GRID_ENERGY, max=Energy.from_watt_hours(100), **_FAST)
    assert value == Energy.from_watt_hours(50.0)
    with pytest.raises(ValueError, match="for_"):
        await site.expect(
            GRID_ENERGY, max=Energy.from_watt_hours(100), for_=timedelta(seconds=1)
        )


async def test_component_expect_rejects_a_non_component_metric() -> None:
    site = _site()
    with pytest.raises(ValueError, match="active_power / soc"):
        await site[3].expect(GRID_POWER, max=Power.from_watts(1))


async def test_component_expect_reads_the_component() -> None:
    site = _site()

    async def fake_active_power(cid: int, mg_id: int | None = None) -> Power | None:
        return Power.from_watts(float(cid))

    site.active_power = fake_active_power  # type: ignore[method-assign]
    value = await site[7].expect(ACTIVE_POWER, max=Power.from_watts(10), **_FAST)
    assert value == Power.from_watts(7.0)


async def test_drive_posts_typed_control_payloads() -> None:
    site = _site()
    calls: list[tuple[str, Any]] = []

    async def fake_control(path: str, payload: Any) -> Any:
        calls.append((path, payload))
        return {}

    site._http.control = fake_control  # type: ignore[method-assign]
    await site[6].drive(power=Power.from_kilowatts(20))
    assert calls == [("/api/component/6/drive", {"power_w": 20000.0})]


async def test_rejected_eval_raises_from_raw_drive() -> None:
    # A RawLisp drive still goes through the eval choke point.
    site = _site()

    async def fake_eval(expr: str, mg_id: int | None = None) -> dict[str, Any]:
        return {"ok": False, "error": "set-meter-power: component 3 not found"}

    site._http.eval = fake_eval  # type: ignore[method-assign]
    with pytest.raises(EvalRejected, match="not found"):
        await site[3].drive(power=sw.raw("(lambda () 100.0)"))
    # EvalRejected still satisfies the historic except ValueError.
    assert issubclass(EvalRejected, ValueError)
