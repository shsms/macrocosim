"""Telemetry faults: what your app's stream of one component delivers.

A component's telemetry mode changes what your app receives on its gRPC stream,
while the component keeps working:

*   ``silent``: the stream stays open, but no samples come;
*   ``error_empty``: samples come with no metrics, only an error state;
*   ``closed``: the stream ends;
*   ``not_found``: the component stays in the component list, but every stream
    of it is refused with ``NOT_FOUND``, so the app keeps retrying.

The Python client reads the simulator's own sampler, so a meter's power read
here does not show the fault; the app's stream and the UI's aggregates do. The
script cycles one meter through each mode for 20 s. Run this script against a
running simulator::

    python examples/faults/telemetry_modes.py
"""

from __future__ import annotations

import asyncio

from frequenz.quantities import Power

import macrocosim as mc

LOAD = mc.make_meter(id=3201, name="load", power=Power.from_kilowatts(12))
MICROGRID = mc.make_microgrid(
    id=32,
    name="Telemetry modes",
    topology=mc.make_grid_connection_point(id=3200, successors=[LOAD]),
)

MODES = (
    mc.TelemetryMode.SILENT,
    mc.TelemetryMode.ERROR_EMPTY,
    mc.TelemetryMode.CLOSED,
    mc.TelemetryMode.NOT_FOUND,
)


async def main() -> None:
    async with mc.connect() as sim:
        microgrid = await sim.declare(MICROGRID)
        load = microgrid[LOAD]
        try:
            for mode in MODES:
                await load.telemetry_mode.set(mode)
                print(f"meter {LOAD.id}: {mode}")
                await asyncio.sleep(20)
        finally:
            try:
                await load.telemetry_mode.set(mc.TelemetryMode.NORMAL)
            except mc.Rejected as exc:
                # The simulator refuses normal when the component's operational
                # mode streams no telemetry; the component then keeps its mode.
                print(f"still not streaming normally: {exc.text}")
            else:
                print("streaming normally again")


if __name__ == "__main__":
    asyncio.run(main())
