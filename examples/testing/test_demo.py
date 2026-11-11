"""Two tests that launch their spec through the conftest fixture.

The spec has a load meter and a battery inverter with its battery, both under
the grid connection point.
"""

from collections.abc import Awaitable, Callable
from datetime import timedelta

from frequenz.quantities import Percentage, Power

import macrocosim as mc

LaunchMicrogrid = Callable[[mc.MicrogridSpec], Awaitable[mc.Microgrid]]

LOAD = mc.make_meter()
BATTERY = mc.make_battery()
SPEC = mc.make_microgrid(
    topology=mc.make_grid_connection_point(
        successors=[LOAD, mc.make_battery_inverter(successors=[BATTERY])],
    ),
)


async def test_soc_set_is_seen(launch_microgrid: LaunchMicrogrid) -> None:
    microgrid = await launch_microgrid(SPEC)
    await microgrid[BATTERY].soc.set(Percentage.from_percent(80))
    await microgrid[BATTERY].soc.expect(
        mc.between(Percentage.from_percent(79), Percentage.from_percent(81)),
        timeout=timedelta(seconds=10),
    )


async def test_grid_power_holds(launch_microgrid: LaunchMicrogrid) -> None:
    microgrid = await launch_microgrid(SPEC)
    await microgrid[LOAD].power.set(Power.from_kilowatts(7))
    await microgrid.grid_power.expect(
        mc.near(Power.from_kilowatts(7), tol=Power.from_kilowatts(1)),
        timeout=timedelta(seconds=30),
        hold_for=timedelta(seconds=5),
    )
