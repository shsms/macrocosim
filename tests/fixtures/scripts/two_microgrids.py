"""Two microgrids, each with a battery under an explicit id; `run` waits until
the engine stops the child. The runner tests edit a copy of this file."""

import asyncio

from frequenz.quantities import Percentage, Power

import macrocosim as mc

NORTH = mc.make_microgrid(
    id=61,
    name="north",
    topology=mc.make_grid_connection_point(
        successors=[
            mc.make_meter(power=Power.from_kilowatts(2)),
            mc.make_battery_inverter(
                id=11,
                successors=[
                    mc.make_battery(id=12, initial_soc=Percentage.from_percent(50))
                ],
            ),
        ]
    ),
)

SOUTH = mc.make_microgrid(
    id=62,
    name="south",
    topology=mc.make_grid_connection_point(
        successors=[
            mc.make_battery_inverter(
                id=21,
                successors=[
                    mc.make_battery(id=22, initial_soc=Percentage.from_percent(50))
                ],
            ),
        ]
    ),
)

MICROGRIDS = [NORTH, SOUTH]


async def run(microgrids: dict[int, mc.Microgrid]) -> None:
    print("running", sorted(microgrids))
    await asyncio.Event().wait()
