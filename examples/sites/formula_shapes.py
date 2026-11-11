"""Unusual topologies, for testing how an app derives its formulas.

An app reads the component graph and works out formulas for the grid, PV,
battery and consumer power from it. These three microgrids each have a shape
that formula code tends to get wrong:

*   **44, a diamond**: one inverter sits under two meters (a component with two
    parents is one spec object in both ``successors`` lists);
*   **45, no grid meter**: the components hang straight off the grid connection
    point, so the grid power is a sum of their meters;
*   **46, a battery under a feeder meter**: the battery inverter shares a meter
    with a load, and the battery's power must not count as consumption.

Open each microgrid's Formulas panel in the UI to see what the simulator's own
formula engine makes of it. Run this script against a running simulator::

    python examples/sites/formula_shapes.py
"""

from __future__ import annotations

import asyncio

from frequenz.quantities import Power

import macrocosim as mc

kW = Power.from_kilowatts

SHARED_PV = mc.make_solar_inverter(id=4404, rated_lower=kW(-10), rated_upper=kW(0))
DIAMOND_MICROGRID = mc.make_microgrid(
    id=44,
    name="Diamond",
    topology=mc.make_grid_connection_point(
        id=4400,
        successors=[
            mc.make_meter(
                id=4401,
                successors=[
                    mc.make_meter(id=4402, successors=[SHARED_PV]),
                    mc.make_meter(id=4403, successors=[SHARED_PV]),
                ],
            )
        ],
    ),
)

NO_GRID_METER_MICROGRID = mc.make_microgrid(
    id=45,
    name="No grid meter",
    topology=mc.make_grid_connection_point(
        id=4500,
        successors=[
            mc.make_meter(id=4501, name="load", power=kW(12)),
            mc.make_meter(
                id=4502,
                successors=[
                    mc.make_battery_inverter(
                        id=4503, successors=[mc.make_battery(id=4504)]
                    )
                ],
            ),
        ],
    ),
)

FEEDER = mc.make_meter(
    id=4601,
    name="feeder",
    successors=[
        mc.make_meter(id=4602, name="load", power=kW(8)),
        mc.make_battery_inverter(id=4603, successors=[mc.make_battery(id=4604)]),
    ],
)
BATTERY_UNDER_FEEDER_MICROGRID = mc.make_microgrid(
    id=46,
    name="Battery under a feeder meter",
    topology=mc.make_grid_connection_point(id=4600, successors=[FEEDER]),
)


async def main() -> None:
    async with mc.connect() as sim:
        await sim.declare(
            DIAMOND_MICROGRID, NO_GRID_METER_MICROGRID, BATTERY_UNDER_FEEDER_MICROGRID
        )
        for microgrid in (
            DIAMOND_MICROGRID,
            NO_GRID_METER_MICROGRID,
            BATTERY_UNDER_FEEDER_MICROGRID,
        ):
            print(f"declared {microgrid.id}: {microgrid.name}")


if __name__ == "__main__":
    asyncio.run(main())
