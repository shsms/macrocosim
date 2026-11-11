"""Two microgrids declared in one call, and the grid frequency every microgrid
shares.

``declare(a, b)`` declares both microgrids in one request and returns both live
microgrids. Each gets its own gRPC server; component ids are unique across the
simulator, so the two sites use different ones. The grid frequency is one value
for the whole simulator: every microgrid reads the same, and either one can
drive it.

The script declares a north and a south site, steps the load of each, and moves
the shared frequency model's nominal for a minute. Run this script against a
running simulator::

    python examples/sites/two_microgrids.py
"""

from __future__ import annotations

import asyncio

from frequenz.quantities import Frequency, Power

import macrocosim as mc

kW = Power.from_kilowatts
Hz = Frequency.from_hertz

NORTH_LOAD = mc.make_meter(id=4101, name="north load", power=kW(15))
SOUTH_LOAD = mc.make_meter(id=4201, name="south load", power=kW(40))
NORTH_MICROGRID = mc.make_microgrid(
    id=41,
    name="North",
    topology=mc.make_grid_connection_point(id=4100, successors=[NORTH_LOAD]),
)
SOUTH_MICROGRID = mc.make_microgrid(
    id=42,
    name="South",
    topology=mc.make_grid_connection_point(id=4200, successors=[SOUTH_LOAD]),
)


async def main() -> None:
    async with mc.connect() as sim:
        north, south = await sim.declare(NORTH_MICROGRID, SOUTH_MICROGRID)
        print("north at", north.grpc_url, "and south at", south.grpc_url)

        await north[NORTH_LOAD].power.set(kW(25))
        await south[SOUTH_LOAD].power.set(kW(10))
        await north.grid_power.expect(mc.near(kW(25), tol=kW(1)))
        await south.grid_power.expect(mc.near(kW(10), tol=kW(1)))
        print("north grid:", await north.grid_power.read())
        print("south grid:", await south.grid_power.read())

        # Lean the shared frequency to 49.8 Hz for a minute: both sites see it.
        try:
            await north.set_frequency_model(nominal=Hz(49.8))
            await asyncio.sleep(60)
        finally:
            await north.set_frequency_model(nominal=Hz(50))
        print("frequency model back at 50 Hz")


if __name__ == "__main__":
    asyncio.run(main())
