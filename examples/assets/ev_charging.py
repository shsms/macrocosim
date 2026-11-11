"""An EV charger, and a car from the simulator's catalog plugged into it.

The charger is configuration: its rating, its phases, and what it offers with no
command standing (``idle``). The car is runtime state: ``plug`` takes one from
the simulator's catalog (``phev``, ``city``, ``sedan``, ``van``) and overrides
its values. Here the charger idles at full power, so a car charges without your
app; it slows down above the car's taper start and stops at its target SoC.

Run this script against a running simulator::

    python examples/assets/ev_charging.py
"""

from __future__ import annotations

import asyncio
from datetime import timedelta

from frequenz.quantities import Energy, Percentage, Power

import macrocosim as mc

kW = Power.from_kilowatts
pct = Percentage.from_percent

CHARGER = mc.make_ev_charger(
    id=1202,
    rated_lower=kW(0),
    rated_upper=kW(11),
    phases=3,
    # Offer the full rating while no command stands; the default, paused, offers
    # nothing until your app commands the charger.
    idle=mc.EvIdle.FULL,
)
MICROGRID = mc.make_microgrid(
    id=12,
    name="EV charging",
    topology=mc.make_grid_connection_point(
        id=1200, successors=[mc.make_meter(id=1201, successors=[CHARGER])]
    ),
)


async def main() -> None:
    async with mc.connect() as sim:
        microgrid = await sim.declare(MICROGRID)
        charger = microgrid[CHARGER]
        # A re-run finds the previous car still plugged in.
        await charger.unplug()
        # A three-phase car (a single-phase one draws only one phase's share of
        # the charger's rating), with a small battery already past its 80 %
        # taper start, so the slowdown shows within a few minutes. Its current
        # falls in a straight line to 20 % of the full current at 100 % SoC.
        await charger.plug(
            "sedan",
            soc=pct(85),
            target_soc=pct(95),
            capacity=Energy.from_kilowatt_hours(5),
        )
        await charger.power.expect(mc.at_least(kW(5)), timeout=timedelta(seconds=20))
        print("charging at", await charger.power.read())
        # Above the taper start, the car draws less and less ...
        await charger.power.expect(mc.at_most(kW(5)), timeout=timedelta(minutes=8))
        print("tapering at", await charger.power.read())
        # ... and nothing once it reaches its target.
        await charger.power.expect(
            mc.at_most(kW(0.1)),
            timeout=timedelta(minutes=10),
            hold_for=timedelta(seconds=5),
        )
        print("full: the car is at its target SoC")
        await charger.unplug()


if __name__ == "__main__":
    asyncio.run(main())
