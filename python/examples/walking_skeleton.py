"""The shortest end-to-end path: build a topology, launch it, set a load, expect
the grid connection point to see it.

Run against a built binary (or with the wheel installed, without
MACROCOSIM_BIN):

    MACROCOSIM_BIN=../target/release/macrocosim python examples/walking_skeleton.py
"""

from __future__ import annotations

import asyncio
from datetime import timedelta

from frequenz.quantities import Power

import macrocosim as mc

# The load meter is the grid connection point's only child, so grid power is
# what the load draws.
LOAD = mc.make_meter(id=11, power=Power.from_kilowatts(7))
SPEC = mc.make_microgrid(
    id=1, topology=mc.make_grid_connection_point(id=10, successors=[LOAD])
)


async def main() -> None:
    async with mc.launch(SPEC) as microgrid:
        print(microgrid)
        await microgrid.grid_power.expect(
            mc.near(Power.from_kilowatts(7), tol=Power.from_watts(100)),
            timeout=timedelta(seconds=30),
        )
        await microgrid[LOAD].power.set(Power.from_kilowatts(12))
        await microgrid.grid_power.expect(
            mc.near(Power.from_kilowatts(12), tol=Power.from_watts(100)),
            hold_for=timedelta(seconds=3),
        )
        print("grid power:", await microgrid.grid_power.read())


if __name__ == "__main__":
    asyncio.run(main())
