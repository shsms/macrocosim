"""End-to-end pass of the signal surface against a real simulator.

Skips when no ``switchyard`` binary is available (``SWITCHYARD_BIN``,
PATH, or the wheel's scripts dir).
"""

from __future__ import annotations

import os
from datetime import timedelta

import pytest
from frequenz.quantities import Energy, Percentage, Power

import switchyard as sw
from switchyard._process import which_binary

pytestmark = [
    pytest.mark.filterwarnings("ignore:.*ComponentId is deprecated.*:DeprecationWarning"),
]

if not (os.environ.get("SWITCHYARD_BIN") or which_binary("switchyard")):
    pytest.skip("no switchyard binary available", allow_module_level=True)

kW = Power.from_kilowatts
kWh = Energy.from_kilowatt_hours
percent = Percentage.from_percent

_SETTLE = timedelta(seconds=20)


async def test_signal_surface_end_to_end() -> None:
    # The builders are the identities — and, once launched, the handles.
    load = sw.meter(id=5, power=Power.zero())
    bat = sw.battery(id=4, capacity=kWh(100), initial_soc=percent(60))
    inv = sw.battery_inverter(id=3, rated=(kW(-50), kW(50)), successors=[bat])
    mg = sw.Microgrid(
        id=1,
        topology=sw.grid(id=1, successors=[sw.meter(id=2, successors=[inv, load])]),
    )

    async with sw.aio.launch(mg) as site:
        # Drive the world through the meter's own signal; watch the
        # aggregate settle through the site's.
        await load.power.set(kW(20))
        await site.grid_power.expect(sw.near(kW(20), tol=kW(1)), timeout=_SETTLE)
        await load.power.expect(sw.near(kW(20), tol=kW(1)), timeout=_SETTLE)

        # State vs flow: stored_energy derives from SoC and capacity...
        soc = await bat.soc.read(wait=timedelta(seconds=10))
        stored = await bat.stored_energy.read()
        expected = kWh(100) * (soc.as_percent() / 100.0)
        assert abs((stored - expected).as_watt_hours()) < 1.0
        # ... while the site's battery_energy is the pool's cumulative flow.
        await site.grid_energy.expect(sw.at_least(Energy.zero()))

        # Teleport the charge state — arranging a precondition, no charging.
        await bat.soc.set(percent(11.0))
        await bat.soc.expect(
            sw.between(percent(10.0), percent(12.0)), timeout=timedelta(seconds=10)
        )

        # Scenario DSL over the same signals: a cue teleports the SoC,
        # a check gates the load — authored pre-launch style, run live.
        scn = sw.Scenario("arrange", length=timedelta(seconds=3))
        scn.at(timedelta(seconds=1), bat.soc, percent(90))
        scn.check(timedelta(seconds=2), load.power, sw.near(kW(20), tol=kW(1)))
        run = await site.define_scenario(scn)
        await run.run(wait=True)
        await run.assert_passed()
        await bat.soc.expect(
            sw.between(percent(88), percent(92)), timeout=timedelta(seconds=10)
        )

        # Fault injection through the typed control API; a bad id raises.
        await inv.health.set(sw.Health.ERROR)
        with pytest.raises(sw.ControlRejected, match="not found"):
            await site[999].drive(power=kW(1))

        # The real gateway path still rejects out-of-envelope setpoints —
        # command() is deliberately not a signal .set.
        await inv.health.set(sw.Health.OK)
        with pytest.raises(sw.SetpointRejected):
            await site[inv].command(active_power=kW(500))

    # The context manager unbinds: the builders are specs again (the
    # identity stays usable; verbs raise).
    with pytest.raises(RuntimeError, match="not bound"):
        await load.power.read()
