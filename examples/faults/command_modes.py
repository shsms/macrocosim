"""Command faults: what your app gets back when it sends a setpoint.

A component's command mode changes how the simulator answers your app's
commands:

*   ``timeout``: no answer; your app's call runs into its deadline;
*   ``error``: ``UNAVAILABLE`` ("component unreachable");
*   ``over_bound``: for one second in each minute, a non-zero setpoint is
    refused with ``INVALID_ARGUMENT``. The error names a limit just below the
    setpoint, although the component's bounds allow it.

The inverter here also has a gateway delay and a ramp, so even a normal setpoint
reaches the output a little later and climbs at 2 kW/s.

The script cycles the battery inverter through the modes, holding each for 70 s
so ``over_bound`` reaches its second. Point your app at it meanwhile, or
``macroctl --microgrid-id 33 set-power 3302 5000`` (macroctl sets no deadline:
stop it with Ctrl+C in ``timeout`` mode). Run this script against a running
simulator::

    python examples/faults/command_modes.py
"""

from __future__ import annotations

import asyncio
import contextlib
from datetime import timedelta

from frequenz.quantities import Power

import macrocosim as mc

kW = Power.from_kilowatts

BATTERY_INVERTER = mc.make_battery_inverter(
    id=3302,
    rated_lower=kW(-20),
    rated_upper=kW(20),
    command_delay=timedelta(seconds=1),
    ramp_rate_w_per_s=2000,
    successors=[mc.make_battery(id=3303)],
)
MICROGRID = mc.make_microgrid(
    id=33,
    name="Command modes",
    topology=mc.make_grid_connection_point(
        id=3300, successors=[mc.make_meter(id=3301, successors=[BATTERY_INVERTER])]
    ),
)

MODES = (mc.CommandMode.TIMEOUT, mc.CommandMode.ERROR, mc.CommandMode.OVER_BOUND)


async def main() -> None:
    async with mc.connect() as sim:
        microgrid = await sim.declare(MICROGRID)
        inverter = microgrid[BATTERY_INVERTER]
        try:
            for mode in MODES:
                await inverter.command_mode.set(mode)
                print(f"inverter {BATTERY_INVERTER.id}: commands answer {mode}")
                await asyncio.sleep(70)
        finally:
            with contextlib.suppress(mc.Rejected):
                await inverter.command_mode.set(mc.CommandMode.NORMAL)
        print("commands answer normally again")


if __name__ == "__main__":
    asyncio.run(main())
