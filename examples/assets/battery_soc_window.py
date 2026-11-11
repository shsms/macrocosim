"""A battery's state of charge, and the SoC window that narrows its bounds.

The battery can hold 0 % to 100 %. The gateway keeps it inside its SoC window,
here 10 % to 90 %. Near either edge, the gateway narrows the battery's bounds
and scales a setpoint on its inverter down to them, so a setpoint your app holds
tapers off and stops at the edge.

This script moves the SoC to a few places in and around the window, so you can
watch the bounds react: open the battery in the UI's inspector, or run
this command::

    macroctl --microgrid-id 11 stream 1103

The script never commands the inverter; your app does that over gRPC.

Run this script against a running simulator::

    python examples/assets/battery_soc_window.py
"""

from __future__ import annotations

import asyncio

from frequenz.quantities import Energy, Percentage, Power

import macrocosim as mc

kW = Power.from_kilowatts
pct = Percentage.from_percent

BATTERY = mc.make_battery(
    id=1103,
    capacity=Energy.from_kilowatt_hours(50),
    initial_soc=pct(50),
    soc_lower=pct(10),
    soc_upper=pct(90),
    # Within this margin of an edge, the bounds taper towards 0.
    soc_protect_margin=pct(5),
    rated_lower=kW(-25),
    rated_upper=kW(25),
)
BATTERY_INVERTER = mc.make_battery_inverter(
    id=1102, rated_lower=kW(-25), rated_upper=kW(25), successors=[BATTERY]
)
MICROGRID = mc.make_microgrid(
    id=11,
    name="Battery SoC window",
    topology=mc.make_grid_connection_point(
        id=1100, successors=[mc.make_meter(id=1101, successors=[BATTERY_INVERTER])]
    ),
)


async def main() -> None:
    async with mc.connect() as sim:
        microgrid = await sim.declare(MICROGRID)
        battery = microgrid[BATTERY]
        for soc in (50, 87, 92, 8, 50):
            # A set is a jump; the physics carries on from the new value.
            await battery.soc.set(pct(soc))
            await battery.soc.expect(mc.between(pct(soc - 1), pct(soc + 1)))
            where = "inside" if 10 <= soc <= 90 else "outside"
            print(f"SoC {soc} % ({where} the window): look at the bounds now")
            await asyncio.sleep(10)


if __name__ == "__main__":
    asyncio.run(main())
