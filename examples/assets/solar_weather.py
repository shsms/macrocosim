"""PV under the site's weather, a passing cloud, and sunlight set by hand.

A solar inverter with no ``sunlight`` in its spec follows the microgrid's
weather: a clear-sky day between sunrise and sunset, plus random clouds. A new
microgrid has no weather, and its inverters get full sun until it has some. Each
inverter sees the weather ``weather_lag`` late (by default 0 to 60 s, picked
from its id), so a cloud sweeps across a site with several arrays. Setting
``sunlight`` takes an inverter off the weather until it is cleared.

The script sets a long day so there is sun at most times of day (least near
midnight UTC), passes one cloud over the site, then drives one array by hand and
hands it back. Watch the PV power in the UI's Metrics panel. Run this script
against a running simulator::

    python examples/assets/solar_weather.py
"""

from __future__ import annotations

import asyncio
from datetime import time, timedelta

from frequenz.quantities import Percentage, Power

import macrocosim as mc

kW = Power.from_kilowatts
pct = Percentage.from_percent

EAST_PV = mc.make_solar_inverter(
    id=1302, rated_lower=kW(-20), rated_upper=kW(0), weather_lag=timedelta(0)
)
WEST_PV = mc.make_solar_inverter(
    id=1304, rated_lower=kW(-20), rated_upper=kW(0), weather_lag=timedelta(seconds=30)
)
MICROGRID = mc.make_microgrid(
    id=13,
    name="Solar and weather",
    topology=mc.make_grid_connection_point(
        id=1300,
        successors=[
            mc.make_meter(id=1301, successors=[EAST_PV]),
            mc.make_meter(id=1303, successors=[WEST_PV]),
        ],
    ),
)


async def main() -> None:
    async with mc.connect() as sim:
        microgrid = await sim.declare(MICROGRID)
        # Sun all day (UTC), and no random clouds: only the one below.
        await microgrid.set_weather(
            sunrise=time(0, 1),
            sunset=time(23, 59),
            peak=pct(90),
            seed=1,
            cloud_mean_gap=timedelta(0),
        )
        print("clear sky: PV", await microgrid.pv_power.read())

        # One cloud: 70 % off the sunlight for a minute, ramping over 10 s. The
        # west array sees it 30 s after the east one.
        await microgrid.pass_cloud(pct(70), timedelta(minutes=1), timedelta(seconds=10))
        await asyncio.sleep(40)
        print("under the cloud: PV", await microgrid.pv_power.read())
        await asyncio.sleep(60)

        # Drive the east array by hand, then give it back to the weather.
        try:
            await microgrid[EAST_PV].sunlight.set(pct(20))
            await asyncio.sleep(20)
            print("east at 20 %: PV", await microgrid.pv_power.read())
        finally:
            await microgrid[EAST_PV].sunlight.clear()
        print("east follows the weather again")


if __name__ == "__main__":
    asyncio.run(main())
