"""The grid's phase voltages, and holding the grid frequency at a set value.

``set_voltage_per_phase`` sets the voltage of each phase; components report it,
and meters and inverters split their power across the phases with it. An
unbalanced grid (one phase low) shows in every meter's per-phase power and
voltage. ``frequency.set`` holds the shared grid frequency at one value until
``frequency.clear`` hands it back to the frequency model.

The script balances the phases, sags L2 for 30 s, then holds the frequency at
49.5 Hz for 30 s. Watch a meter's per-phase readings in the UI's inspector. Run
this script against a running simulator::

    python examples/sites/grid_voltage.py
"""

from __future__ import annotations

import asyncio

from frequenz.quantities import Frequency, Power, Voltage

import macrocosim as mc

V = Voltage.from_volts

LOAD = mc.make_meter(id=4301, name="load", power=Power.from_kilowatts(20))
MICROGRID = mc.make_microgrid(
    id=43,
    name="Grid voltage",
    topology=mc.make_grid_connection_point(id=4300, successors=[LOAD]),
)


async def main() -> None:
    async with mc.connect() as sim:
        microgrid = await sim.declare(MICROGRID)
        try:
            await microgrid.set_voltage_per_phase(V(230), V(230), V(230))
            print("balanced: 230 V on each phase")
            await asyncio.sleep(10)
            await microgrid.set_voltage_per_phase(V(230), V(207), V(230))
            print("L2 sags to 207 V for 30 s")
            await asyncio.sleep(30)
        finally:
            await microgrid.set_voltage_per_phase(V(230), V(230), V(230))

        try:
            await microgrid.frequency.set(Frequency.from_hertz(49.5))
            print("frequency held at 49.5 Hz for 30 s")
            await asyncio.sleep(30)
        finally:
            await microgrid.frequency.clear()
        print("frequency follows its model again")


if __name__ == "__main__":
    asyncio.run(main())
