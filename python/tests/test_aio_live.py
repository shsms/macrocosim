"""End-to-end pass of the signal surface against a real simulator.

Skips when no ``macrocosim`` binary is available (``MACROCOSIM_BIN``,
PATH, or the wheel's scripts dir).
"""

from __future__ import annotations

import asyncio
import os
import time
from datetime import timedelta

import pytest
from frequenz.quantities import Energy, Percentage, Power

import macrocosim as mc
from macrocosim._process import which_binary

pytestmark = [
    pytest.mark.filterwarnings("ignore:.*ComponentId is deprecated.*:DeprecationWarning"),
]

if not (os.environ.get("MACROCOSIM_BIN") or which_binary("macrocosim")):
    pytest.skip("no macrocosim binary available", allow_module_level=True)

kW = Power.from_kilowatts
kWh = Energy.from_kilowatt_hours
percent = Percentage.from_percent

_SETTLE = timedelta(seconds=20)


async def test_signal_surface_end_to_end() -> None:
    # The builders are the identities — and, once launched, the handles.
    load = mc.meter(id=5, power=Power.zero())
    bat = mc.battery(id=4, capacity=kWh(100), initial_soc=percent(60))
    inv = mc.battery_inverter(id=3, rated=(kW(-50), kW(50)), successors=[bat])
    mg = mc.Microgrid(
        id=1,
        topology=mc.grid(id=1, successors=[mc.meter(id=2, successors=[inv, load])]),
    )

    async with mc.aio.launch(mg) as site:
        # Drive the world through the meter's own signal; watch the
        # aggregate settle through the site's.
        await load.power.set(kW(20))
        await site.grid_power.expect(mc.near(kW(20), tol=kW(1)), timeout=_SETTLE)
        await load.power.expect(mc.near(kW(20), tol=kW(1)), timeout=_SETTLE)

        # State vs flow: stored_energy derives from SoC and capacity...
        soc = await bat.soc.read(wait=timedelta(seconds=10))
        stored = await bat.stored_energy.read()
        expected = kWh(100) * (soc.as_percent() / 100.0)
        assert abs((stored - expected).as_watt_hours()) < 1.0
        # ... while the site's battery_energy is the pool's cumulative flow.
        await site.grid_energy.expect(mc.at_least(Energy.zero()))

        # Teleport the charge state — arranging a precondition, no charging.
        await bat.soc.set(percent(11.0))
        await bat.soc.expect(
            mc.between(percent(10.0), percent(12.0)), timeout=timedelta(seconds=10)
        )

        # Scenario DSL over the same signals: a cue teleports the SoC,
        # a check gates the load — authored pre-launch style, run live.
        scn = mc.Scenario("arrange", length=timedelta(seconds=3))
        scn.at(timedelta(seconds=1), bat.soc, percent(90))
        scn.check(timedelta(seconds=2), load.power, mc.near(kW(20), tol=kW(1)))
        run = await site.define_scenario(scn)
        await run.run(wait=True)
        await run.assert_passed()
        await bat.soc.expect(
            mc.between(percent(88), percent(92)), timeout=timedelta(seconds=10)
        )

        # Fault injection through the typed control API; a bad id raises.
        await inv.health.set(mc.Health.ERROR)
        with pytest.raises(mc.ControlRejected, match="not found"):
            await site[999].drive(power=kW(1))

        # The real gateway path still rejects out-of-envelope setpoints —
        # command() is deliberately not a signal .set.
        await inv.health.set(mc.Health.OK)
        with pytest.raises(mc.SetpointRejected):
            await site[inv].command(active_power=kW(500))

    # The context manager unbinds: the builders are specs again (the
    # identity stays usable; verbs raise).
    with pytest.raises(RuntimeError, match="not bound"):
        await load.power.read()


async def _read_until(
    site: mc.aio.Site,
    component_id: int,
    *,
    a_number: bool,
    limit: timedelta = timedelta(seconds=10),
) -> Power | None:
    """Poll a component's gRPC active power until it is (or stops being) a number.

    Each read may itself wait for a first sample, so this bounds the whole
    wait rather than the number of attempts.
    """
    deadline = time.monotonic() + limit.total_seconds()
    while True:
        value = await site.active_power(component_id)
        if (value is not None) == a_number:
            return value
        if time.monotonic() >= deadline:
            raise AssertionError(
                f"component {component_id} active power stayed {value!r} for {limit}"
            )
        await asyncio.sleep(0.2)


async def test_a_closed_telemetry_stream_recovers_once_the_fault_clears() -> None:
    """The CLOSED telemetry fault ends the component's stream.

    Reads must go to None rather than serving the last pre-fault sample,
    and once the fault is cleared a read must open a new stream and see
    live data again.
    """
    load = mc.meter(id=5, power=kW(20))
    mg = mc.Microgrid(
        id=1,
        topology=mc.grid(id=1, successors=[mc.meter(id=2, successors=[load])]),
    )

    async with mc.aio.launch(mg) as site:
        assert await _read_until(site, 5, a_number=True) is not None

        # The server closes the component's telemetry stream.
        await site[5].status(telemetry_mode=mc.TelemetryMode.CLOSED)
        assert await _read_until(site, 5, a_number=False) is None

        # Clearing the fault must be enough: the next reads open a new
        # stream instead of retrying the closed one.
        await site[5].status(telemetry_mode=mc.TelemetryMode.NORMAL)
        assert await _read_until(site, 5, a_number=True) is not None
