"""The 0.2 client against a real simulator (private imports until the switch)."""

from __future__ import annotations

import os
import time
from datetime import timedelta
from pathlib import Path
from typing import Any

import pytest
from frequenz.quantities import (
    ApparentPower,
    Current,
    Energy,
    Percentage,
    Power,
    ReactivePower,
    Voltage,
)
from topology_check import topology_problems

import macrocosim._spec as s
from macrocosim._enums import CommandMode, Health, OperationalMode, TelemetryMode
from macrocosim._errors import MicrogridClosed, NoSample
from macrocosim._launch import launch
from macrocosim._matchers import at_least, at_most, between, near

pytestmark = [
    pytest.mark.live,
    pytest.mark.filterwarnings("error"),
    # Deprecations inside frequenz-client-microgrid, which only
    # test_grpc_url_reachable imports; listed after "error" so they win.
    pytest.mark.filterwarnings(
        "ignore:frequenz.client.common.microgrid.components.ComponentId is "
        "deprecated:DeprecationWarning"
    ),
    pytest.mark.filterwarnings("ignore:GRID is deprecated"),
    pytest.mark.filterwarnings("ignore:Please use BroadcastChannel instead"),
]

kW = Power.from_kilowatts
W = Power.from_watts
pct = Percentage.from_percent
sec = timedelta(seconds=1)


def _downstream_like() -> tuple[s.MicrogridSpec, s.MeterSpec, Any, Any]:
    battery = s.make_battery(
        id=1003,
        capacity=Energy.from_kilowatt_hours(100),
        initial_soc=pct(50),
        soc_lower=pct(10),
        soc_upper=pct(90),
    )
    inverter = s.make_battery_inverter(
        id=1002, rated_lower=kW(-30), rated_upper=kW(30), successors=[battery]
    )
    load = s.make_meter(id=1004, power=kW(10))
    spec = s.make_microgrid(
        id=7,
        topology=s.make_grid_connection_point(
            id=1000,
            successors=[s.make_meter(id=1001, successors=[inverter]), load],
        ),
    )
    return spec, load, inverter, battery


async def test_downstream_flow(tmp_path: Path) -> None:
    spec, load, inverter, battery = _downstream_like()
    async with launch(spec, state_dir=tmp_path / "mc") as mg:
        await mg[load].power.set(kW(20))
        await mg.grid_power.expect(near(kW(20), tol=kW(1)), timeout=20 * sec)
        await mg[load].power.expect(near(kW(20), tol=kW(1)))
        assert await mg[inverter].power.read() == W(0)
        assert pct(0) <= await mg[battery].soc.read() <= pct(100)
        await mg.grid_energy.expect(at_least(Energy.from_watt_hours(0)))
        await mg[battery].soc.set(pct(11))
        await mg[battery].soc.expect(between(pct(10), pct(12)), hold_for=3 * sec)
        await mg[inverter].health.set(Health.ERROR)
        assert mg.log_path == tmp_path / "mc" / "macrocosim.log"
    with pytest.raises(MicrogridClosed):
        await mg[load].power.read()


async def test_clear() -> None:
    load = s.make_meter(
        id=11, power=kW(5), reactive_power=ReactivePower.from_volt_amperes_reactive(500)
    )
    solar = s.make_solar_inverter(id=12, sunlight=pct(80))
    spec = s.make_microgrid(
        id=7, topology=s.make_grid_connection_point(id=10, successors=[load, solar])
    )
    async with launch(spec) as mg:
        await mg[load].power.expect(near(kW(5), tol=W(10)))
        await mg[load].power.clear()
        await mg[load].power.expect(near(W(0), tol=W(10)))
        await mg[load].reactive_power.expect(
            near(
                ReactivePower.from_volt_amperes_reactive(500),
                tol=ReactivePower.from_volt_amperes_reactive(10),
            )
        )
        await mg[load].reactive_power.clear()
        await mg[load].reactive_power.expect(
            near(
                ReactivePower.from_volt_amperes_reactive(0),
                tol=ReactivePower.from_volt_amperes_reactive(10),
            )
        )
        await mg[solar].power.expect(at_most(W(-100)), timeout=20 * sec)
        await mg[solar].sunlight.set(pct(0))
        await mg[solar].power.expect(near(W(0), tol=W(10)), timeout=20 * sec)
        await mg[solar].sunlight.clear()  # follows the weather again; no error


def _knobs() -> dict[str, Any]:
    return {
        "operational_mode": OperationalMode.CONTROL_AND_TELEMETRY,
        "health": Health.OK,
        "telemetry_mode": TelemetryMode.NORMAL,
        "command_mode": CommandMode.NORMAL,
        "stream_jitter": pct(2),
    }


def _every_field() -> s.MicrogridSpec:
    def common(cid: int) -> dict[str, Any]:
        return {"id": cid, "name": f"c{cid}", **_knobs()}

    battery = s.make_battery(
        **common(104),
        interval=2 * sec,
        capacity=Energy.from_kilowatt_hours(50),
        initial_soc=pct(60),
        soc_lower=pct(20),
        soc_upper=pct(80),
        soc_protect_margin=pct(5),
        voltage=Voltage.from_volts(700),
        rated_lower=kW(-20),
        rated_upper=kW(20),
    )
    inverter = s.make_battery_inverter(
        **common(103),
        interval=2 * sec,
        successors=[battery],
        rated_lower=kW(-20),
        rated_upper=kW(20),
        command_delay=sec,
        device_delay=timedelta(milliseconds=200),
        reactive_command_delay=timedelta(milliseconds=200),
        ramp_rate_w_per_s=4000.0,
        reactive_pf_limit=0.5,
        reactive_apparent=ApparentPower.from_kilo_volt_amperes(25),
        reactive_ramp_rate_var_per_s=1000.0,
    )
    solar = s.make_solar_inverter(
        **common(105),
        interval=2 * sec,
        rated_lower=kW(-10),
        rated_upper=W(0),
        command_delay=timedelta(milliseconds=500),
        device_delay=timedelta(milliseconds=200),
        reactive_command_delay=timedelta(milliseconds=200),
        ramp_rate_w_per_s=1000.0,
        reactive_pf_limit=0.3,
        reactive_apparent=ApparentPower.from_kilo_volt_amperes(12),
        reactive_ramp_rate_var_per_s=1000.0,
        sunlight=pct(50),
        array_peak=kW(12),
        weather_lag=10 * sec,
        weather_jitter=pct(5),
    )
    leaf = s.make_meter(
        **common(102),
        interval=timedelta(milliseconds=500),
        power=kW(2),
        power_factor=0.9,
        leading=True,
        hidden=True,
    )
    chp = s.make_chp(**common(106))
    main = s.make_meter(
        **common(101),
        interval=timedelta(milliseconds=500),
        power=kW(10),
        reactive_power=ReactivePower.from_kilo_volt_amperes_reactive(1),
        hidden=False,
        successors=[leaf, inverter, solar, chp],
    )
    return s.make_microgrid(
        id=9,
        name="every-field",
        tso="tso-test",
        topology=s.make_grid_connection_point(
            **common(100),
            rated_fuse_current=Current.from_amperes(200),
            rated_lower=kW(-100),
            rated_upper=kW(100),
            successors=[main],
        ),
    )


async def _topology(mg: Any) -> dict[str, Any]:
    return await mg._session.get_json(f"/api/mg/{mg.id}/topology")


async def test_every_field_boots() -> None:
    spec = _every_field()
    async with launch(spec) as mg:
        assert mg.id == 9
        assert topology_problems(spec, await _topology(mg)) == []


def _diamond() -> s.MicrogridSpec:
    inv = s.make_battery_inverter(successors=[s.make_battery()])
    return s.make_microgrid(
        id=7,
        topology=s.make_grid_connection_point(
            successors=[s.make_meter(successors=[inv]), s.make_meter(successors=[inv])]
        ),
    )


def _shared_battery() -> s.MicrogridSpec:
    bat = s.make_battery()
    return s.make_microgrid(
        id=7,
        topology=s.make_grid_connection_point(
            successors=[
                s.make_meter(
                    successors=[
                        s.make_battery_inverter(successors=[bat]),
                        s.make_battery_inverter(successors=[bat]),
                    ]
                )
            ]
        ),
    )


def _partial_ids() -> s.MicrogridSpec:
    return s.make_microgrid(
        id=7,
        topology=s.make_grid_connection_point(
            id=1,
            successors=[
                s.make_meter(
                    successors=[s.make_battery_inverter(successors=[s.make_battery()])]
                ),
                s.make_meter(id=1000),
                s.make_meter(),
            ],
        ),
    )


@pytest.mark.parametrize("build", [_diamond, _shared_battery, _partial_ids])
async def test_topology_matches_spec(build: Any) -> None:
    spec = build()
    async with launch(spec) as mg:
        assert topology_problems(spec, await _topology(mg)) == []


async def test_units() -> None:
    spec, load, inverter, battery = _downstream_like()
    async with launch(spec) as mg:
        session = mg._session
        for cid, metric, unit in [
            (1004, "active_power_w", "W"),
            (1004, "reactive_power_var", "VAr"),
            (1002, "active_power_w", "W"),
            (1003, "soc_pct", "%"),
        ]:
            body = await session.get_json(
                f"/api/mg/7/component/{cid}/history",
                {"metric": metric, "window_s": 3},
            )
            assert body["unit"] == unit, (cid, metric)
        await mg.grid_power.read(timeout=10 * sec)
        await mg.grid_energy.read(timeout=10 * sec)
        latest = await session.get_json("/api/mg/7/metrics/latest")
        for stream, unit in [
            ("grid_power", "W"),
            ("consumer_power", "W"),
            ("battery_pool_power", "W"),
            ("pv_power", "W"),
            ("grid_energy", "Wh"),
            ("consumer_energy", "Wh"),
            ("battery_pool_energy", "Wh"),
            ("pv_energy", "Wh"),
        ]:
            assert latest[stream]["unit"] == unit, stream


async def test_an_absent_category_reads_zero() -> None:
    spec = s.make_microgrid(
        id=7,
        topology=s.make_grid_connection_point(successors=[s.make_meter(power=kW(5))]),
    )
    async with launch(spec) as mg:
        assert await mg.pv_power.read(timeout=10 * sec) == W(0)
        assert await mg.battery_power.read(timeout=10 * sec) == W(0)


async def test_read_asymmetry() -> None:
    meter = s.make_meter(power=kW(5))
    spec = s.make_microgrid(
        id=7, topology=s.make_grid_connection_point(successors=[meter])
    )
    async with launch(spec) as mg:
        await mg.grid_power.expect(near(kW(5), tol=W(100)), timeout=20 * sec)
        await mg[meter].telemetry_mode.set(TelemetryMode.SILENT)
        # The meter's own reads come from the sampler and do not see the fault.
        assert await mg[meter].power.read() == kW(5)
        started = time.monotonic()
        while True:
            try:
                await mg.grid_power.read(timeout=sec)
            except NoSample:
                break
            assert time.monotonic() - started < 5, "grid_power kept producing values"
        await mg[meter].telemetry_mode.set(TelemetryMode.NORMAL)
        assert await mg.grid_power.read(timeout=10 * sec) == kW(5)


async def test_grpc_url_reachable() -> None:
    if os.environ.get("MACROCOSIM_REQUIRE_LIVE") == "1":
        import frequenz.client.microgrid as client_module
    else:
        client_module = pytest.importorskip("frequenz.client.microgrid")
    spec, *_ = _downstream_like()
    async with launch(spec) as mg:
        client = client_module.MicrogridApiClient(mg.grpc_url)
        try:
            ids = {int(c.id) for c in await client.list_components()}
        finally:
            await client.disconnect()
    assert ids == {1000, 1001, 1002, 1003, 1004}
