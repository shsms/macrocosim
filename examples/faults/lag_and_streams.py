"""Sample timestamps that lag behind, and every telemetry stream cancelled at
once.

``set_sample_lag`` shifts the timestamp of every sample your app receives into
the past, as from a slow gateway, so the app can be tested for how it treats old
data. ``cancel_streams`` ends every telemetry stream the app has open with
``CANCELLED``, as on a network hiccup; the app has to open them again.

The script turns a 5-second lag on and off, then cancels the streams twice.
Point your app at microgrid 34 meanwhile. Run this script against a running
simulator::

    python examples/faults/lag_and_streams.py
"""

from __future__ import annotations

import asyncio
from datetime import timedelta

from frequenz.quantities import Power

import macrocosim as mc

LOAD = mc.make_meter(id=3401, name="load", power=Power.from_kilowatts(8))
MICROGRID = mc.make_microgrid(
    id=34,
    name="Lag and streams",
    topology=mc.make_grid_connection_point(id=3400, successors=[LOAD]),
)


async def main() -> None:
    async with mc.connect() as sim:
        microgrid = await sim.declare(MICROGRID)
        try:
            await microgrid.set_sample_lag(timedelta(seconds=5))
            print("samples arrive 5 s old for 30 s")
            await asyncio.sleep(30)
        finally:
            await microgrid.set_sample_lag(timedelta(0))
        print("samples are current again")

        for _ in range(2):
            await asyncio.sleep(15)
            await microgrid.cancel_streams()
            print("cancelled every open telemetry stream")


if __name__ == "__main__":
    asyncio.run(main())
