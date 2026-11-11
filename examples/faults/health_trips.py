"""Setting a component's health to ``error`` or ``standby``, and back.

A component set to ``error`` (or ``standby``) trips offline: an inverter's
output snaps to 0, its telemetry reports that state, and commands to it are
refused: with ``UNAVAILABLE`` in ``error``, ``FAILED_PRECONDITION`` in
``standby``. A battery inverter also clears its setpoint, so your app has to
command it again after recovery. A PV inverter resumes from the sunlight on its
own.

The script trips the PV inverter and the battery inverter in turn and brings
them back, printing the PV power on the way. Point your app at microgrid 31 to
see how it reacts. Run this script against a running simulator::

    python examples/faults/health_trips.py
"""

from __future__ import annotations

import asyncio
from datetime import timedelta

from frequenz.quantities import Percentage, Power

import macrocosim as mc

kW = Power.from_kilowatts

SOLAR = mc.make_solar_inverter(
    id=3102, rated_lower=kW(-20), rated_upper=kW(0), sunlight=Percentage.from_percent(80)
)
BATTERY_INVERTER = mc.make_battery_inverter(
    id=3104,
    rated_lower=kW(-20),
    rated_upper=kW(20),
    successors=[mc.make_battery(id=3105)],
)
MICROGRID = mc.make_microgrid(
    id=31,
    name="Health trips",
    topology=mc.make_grid_connection_point(
        id=3100,
        successors=[
            mc.make_meter(id=3101, successors=[SOLAR]),
            mc.make_meter(id=3103, successors=[BATTERY_INVERTER]),
        ],
    ),
)


async def main() -> None:
    async with mc.connect() as sim:
        microgrid = await sim.declare(MICROGRID)
        solar = microgrid[SOLAR]
        battery_inverter = microgrid[BATTERY_INVERTER]
        try:
            print("PV before the trip:", await solar.power.read())
            await solar.health.set(mc.Health.ERROR)
            await solar.power.expect(
                mc.near(kW(0), tol=kW(0.1)), timeout=timedelta(seconds=10)
            )
            print("PV tripped:", await solar.power.read())
            await asyncio.sleep(20)
            await solar.health.set(mc.Health.OK)
            await solar.power.expect(mc.at_most(kW(-5)), timeout=timedelta(seconds=30))
            print("PV back:", await solar.power.read())

            # Standby takes the battery inverter out as well; your app has to
            # command it again once it is back.
            await battery_inverter.health.set(mc.Health.STANDBY)
            print("battery inverter in standby for 30 s")
            await asyncio.sleep(30)
        finally:
            await solar.health.set(mc.Health.OK)
            await battery_inverter.health.set(mc.Health.OK)
        print("both healthy again")


if __name__ == "__main__":
    asyncio.run(main())
