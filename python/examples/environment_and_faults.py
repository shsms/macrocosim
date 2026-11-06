"""Change the simulated world, inject a fault, and see how reads differ.

Component reads come from the simulator's own sampler, so they do not show
telemetry faults. Aggregates such as ``grid_power`` come from the gRPC
telemetry, so they do.

The test changes the environment (sunlight, a battery's state of charge, a load)
and faults; the app under test, which this example leaves out, would command the
inverters over ``microgrid.grpc_url``.

    MACROCOSIM_BIN=../target/release/macrocosim python examples/environment_and_faults.py
"""

from __future__ import annotations

import asyncio
from datetime import timedelta

from frequenz.quantities import Energy, Percentage, Power

import macrocosim as mc

kW = Power.from_kilowatts
pct = Percentage.from_percent

BATTERY = mc.make_battery(
    id=14, capacity=Energy.from_kilowatt_hours(100), initial_soc=pct(50)
)
INVERTER = mc.make_battery_inverter(id=13, successors=[BATTERY])
SOLAR = mc.make_solar_inverter(id=15, sunlight=pct(80), array_peak=kW(10))
LOAD = mc.make_meter(id=12, power=kW(5))
MAIN = mc.make_meter(id=11, successors=[LOAD, INVERTER, SOLAR])
SPEC = mc.make_microgrid(
    id=1, topology=mc.make_grid_connection_point(id=10, successors=[MAIN])
)


async def main() -> None:
    async with mc.launch(SPEC) as microgrid:
        # Sunlight is the power available: 80 % of a 10 kW array.
        await microgrid[SOLAR].power.expect(
            mc.near(kW(-8), tol=kW(0.5)), timeout=timedelta(seconds=30)
        )
        await microgrid[SOLAR].sunlight.set(pct(20))
        await microgrid[SOLAR].power.expect(mc.near(kW(-2), tol=kW(0.5)))

        # A state of charge set is a one-time jump; the physics carries on.
        await microgrid[BATTERY].soc.set(pct(90))
        await microgrid[BATTERY].soc.expect(mc.between(pct(89), pct(91)))

        # Silence the main meter: the app (and grid_power, which is computed
        # from the same telemetry) stops seeing it, but this test's own read of
        # the meter keeps returning values. That is a current limitation.
        await microgrid[MAIN].telemetry_mode.set(mc.TelemetryMode.SILENT)
        print("main meter still reads:", await microgrid[MAIN].power.read())
        # The aggregate reuses the meter's last sample for a few seconds, then
        # has no value.
        while True:
            try:
                await microgrid.grid_power.read(timeout=timedelta(seconds=1))
            except mc.NoSample:
                print("grid_power has no value while the meter is silent")
                break
        await microgrid[MAIN].telemetry_mode.set(mc.TelemetryMode.NORMAL)

        # A tripped inverter goes to 0 W.
        await microgrid[SOLAR].health.set(mc.Health.ERROR)
        await microgrid[SOLAR].power.expect(mc.near(kW(0), tol=kW(0.1)))


if __name__ == "__main__":
    asyncio.run(main())
