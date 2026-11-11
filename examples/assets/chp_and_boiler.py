"""A CHP and a hybrid steam boiler.

A CHP has no physics of its own: it marks what the meter above it measures, so
its power is that meter's power (negative: it produces).

The steam boiler is a hybrid. Your app commands its electric power, and a gas
burner keeps the pressure from falling below its target. The site draws steam
(``demand``); with no electric command, the gas burner covers all of it, and the
pressure stays at the target.

The script changes the steam demand, then raises the pressure 1 bar above the
target once: with no electric power, the steam the site draws brings it back
down to the target in about three minutes. Run this script against a running
simulator::

    python examples/assets/chp_and_boiler.py
"""

from __future__ import annotations

import asyncio
from datetime import timedelta

from frequenz.quantities import Power

import macrocosim as mc

kW = Power.from_kilowatts
bar = mc.Pressure.from_bar
kg_per_s = mc.MassFlow.from_kilograms_per_second

CHP_METER = mc.make_meter(id=1401, power=kW(-15), successors=[mc.make_chp(id=1402)])
BOILER = mc.make_steam_boiler(
    id=1404,
    rated_lower=kW(0),
    rated_upper=kW(200),
    target=bar(10),
    max=bar(14),
    initial=bar(10),
    demand=kg_per_s(0.02),
)
MICROGRID = mc.make_microgrid(
    id=14,
    name="CHP and steam boiler",
    topology=mc.make_grid_connection_point(
        id=1400, successors=[CHP_METER, mc.make_meter(id=1403, successors=[BOILER])]
    ),
)


async def main() -> None:
    async with mc.connect() as sim:
        microgrid = await sim.declare(MICROGRID)
        chp_meter = microgrid[CHP_METER]
        boiler = microgrid[BOILER]

        # The CHP's output is its meter's power: 25 kW now.
        await chp_meter.power.set(kW(-25))
        print("CHP meter:", await chp_meter.power.read())

        # The gas burner covers any demand; the pressure stays at its target.
        for demand in (0.05, 0.01, 0.02):
            await boiler.demand.set(kg_per_s(demand))
            print(f"steam demand {demand} kg/s; pressure", await boiler.pressure.read())
            await asyncio.sleep(20)

        # Pressure above the target: the steam drawn brings it back down.
        await boiler.demand.set(kg_per_s(0.1))
        await boiler.pressure.set(bar(11))
        print("pressure raised to", await boiler.pressure.read())
        await boiler.pressure.expect(mc.at_most(bar(10.1)), timeout=timedelta(minutes=5))
        print("back at the target:", await boiler.pressure.read())
        await boiler.demand.set(kg_per_s(0.02))


if __name__ == "__main__":
    asyncio.run(main())
