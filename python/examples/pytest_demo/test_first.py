"""A first test: set a load and expect the grid connection point to see it."""

from datetime import timedelta

from frequenz.quantities import Power

import macrocosim as mc


async def test_grid_power_follows_the_load() -> None:
    load = mc.make_meter()
    spec = mc.make_microgrid(topology=mc.make_grid_connection_point(successors=[load]))

    async with mc.launch(spec) as microgrid:
        await microgrid[load].power.set(Power.from_kilowatts(20))
        await microgrid.grid_power.expect(
            mc.near(Power.from_kilowatts(20), tol=Power.from_kilowatts(1)),
            timeout=timedelta(seconds=30),
        )
