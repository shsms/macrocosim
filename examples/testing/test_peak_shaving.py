"""Testing an app with pytest: a peak-shaving check.

Each test launches its own simulator with ``mc.launch``, starts your app against
the microgrid's gRPC address, drives the environment, and checks the outcome
with ``expect``. The app is yours: set ``APP_CMD`` to the command that starts
it, with ``{grpc_url}`` where the address goes. Without ``APP_CMD`` the test is
skipped::

    APP_CMD="my-peak-shaver --microgrid {grpc_url} --limit-kw 30" \\
        pytest examples/testing/test_peak_shaving.py

The app should keep the grid import at or below 30 kW by discharging the battery
while the load is above that, and leave the battery idle while the import is
under the limit.
"""

from __future__ import annotations

import asyncio
import os
import shlex
from collections.abc import AsyncIterator, Awaitable, Callable
from datetime import timedelta

import pytest
from frequenz.quantities import Energy, Percentage, Power

import macrocosim as mc

kW = Power.from_kilowatts

LOAD = mc.make_meter(id=2101, name="load")
BATTERY = mc.make_battery(
    id=2104,
    capacity=Energy.from_kilowatt_hours(100),
    initial_soc=Percentage.from_percent(80),
    rated_lower=kW(-50),
    rated_upper=kW(50),
)
BATTERY_INVERTER = mc.make_battery_inverter(
    id=2103, rated_lower=kW(-50), rated_upper=kW(50), successors=[BATTERY]
)
MICROGRID = mc.make_microgrid(
    id=21,
    name="Peak shaving",
    topology=mc.make_grid_connection_point(
        id=2100,
        successors=[LOAD, mc.make_meter(id=2102, successors=[BATTERY_INVERTER])],
    ),
)

LaunchMicrogrid = Callable[[mc.MicrogridSpec], Awaitable[mc.Microgrid]]

APP_CMD = os.environ.get("APP_CMD")


@pytest.fixture
async def microgrid(launch_microgrid: LaunchMicrogrid) -> AsyncIterator[mc.Microgrid]:
    """The microgrid in its own simulator (conftest.py), with your app running
    against it; both stop when the test ends."""
    if not APP_CMD:
        pytest.skip("set APP_CMD to the command that starts your app")
    launched = await launch_microgrid(MICROGRID)
    args = shlex.split(APP_CMD.replace("{grpc_url}", launched.grpc_url))
    app = await asyncio.create_subprocess_exec(*args)
    try:
        yield launched
        assert app.returncode is None, f"your app exited early, with {app.returncode}"
    finally:
        if app.returncode is None:
            app.terminate()
            try:
                await asyncio.wait_for(app.wait(), timeout=10)
            except TimeoutError:
                app.kill()
                await app.wait()


async def test_grid_import_stays_under_the_limit(microgrid: mc.Microgrid) -> None:
    await microgrid[LOAD].power.set(kW(20))
    await microgrid.grid_power.expect(mc.near(kW(20), tol=kW(2)))
    # Under the limit, the app leaves the battery idle.
    await microgrid.battery_power.expect(
        mc.near(kW(0), tol=kW(1)), hold_for=timedelta(seconds=10)
    )

    # A peak: 45 kW of load. Once the grid shows it (above 25 kW, whether the
    # app has already shaved it to 30 kW or not), the app has a minute to bring
    # the grid under 30 kW, and must hold it there. Expecting the limit straight
    # after the set could match a grid sample from before the peak.
    await microgrid[LOAD].power.set(kW(45))
    await microgrid.grid_power.expect(mc.at_least(kW(25)))
    await microgrid.grid_power.expect(
        mc.at_most(kW(30.5)),
        timeout=timedelta(minutes=1),
        hold_for=timedelta(seconds=30),
    )
    await microgrid.battery_power.expect(mc.at_most(kW(-14)))
