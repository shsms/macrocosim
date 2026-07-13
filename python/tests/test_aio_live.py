"""End-to-end pass of the async core against a real simulator.

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
from switchyard.metrics import ACTIVE_POWER, GRID_ENERGY, GRID_POWER, SOC

pytestmark = [
    pytest.mark.filterwarnings("ignore:.*ComponentId is deprecated.*:DeprecationWarning"),
]

if not (os.environ.get("SWITCHYARD_BIN") or which_binary("switchyard")):
    pytest.skip("no switchyard binary available", allow_module_level=True)

kW = Power.from_kilowatts

GRID_ID = 1
METER_ID = 2
INVERTER_ID = 3
BATTERY_ID = 4
LOAD_ID = 5


def _topology() -> sw.Microgrid:
    return sw.Microgrid(
        id=1,
        topology=sw.grid(
            id=GRID_ID,
            successors=[
                sw.meter(
                    id=METER_ID,
                    successors=[
                        sw.battery_inverter(
                            id=INVERTER_ID,
                            rated=(kW(-50), kW(50)),
                            successors=[
                                sw.battery(
                                    id=BATTERY_ID,
                                    capacity=Energy.from_kilowatt_hours(100),
                                    soc=Percentage.from_percent(60),
                                )
                            ],
                        ),
                        sw.meter(id=LOAD_ID, power=Power.zero()),
                    ],
                )
            ],
        ),
    )


async def test_async_core_end_to_end() -> None:
    async with sw.aio.launch(_topology()) as site:
        # Drive the environment and watch the ground truth settle.
        await site[LOAD_ID].drive(power=kW(20))
        await site.expect(
            GRID_POWER,
            approx=kW(20),
            tol=kW(1),
            timeout=timedelta(seconds=20),
        )

        # Component reads + expects over the native async gRPC path.
        await site[LOAD_ID].expect(
            ACTIVE_POWER,
            approx=kW(20),
            tol=kW(1),
            timeout=timedelta(seconds=20),
        )
        soc = await site[BATTERY_ID].soc()
        assert soc is not None
        await site[BATTERY_ID].expect(
            SOC,
            within=(Percentage.from_percent(50), Percentage.from_percent(70)),
        )

        # A cumulative metric: the total exists and only grows.
        energy = await site.expect(GRID_ENERGY, min=Energy.from_watt_hours(0))
        assert energy is not None

        # Fault injection through the typed choke point; a bad id raises.
        await site[INVERTER_ID].status(health=sw.Health.ERROR)
        with pytest.raises(ValueError, match="not found"):
            await site[999].drive(power=kW(1))

        # The real gateway path still rejects out-of-envelope setpoints.
        await site[INVERTER_ID].status(health=sw.Health.OK)
        with pytest.raises(sw.SetpointRejected):
            await site[INVERTER_ID].command(active_power=kW(500))
