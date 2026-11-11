"""The UI smoke's site: microgrid 2200 with a battery, PV, an EV charger, a CHP
and a hidden consumer load, and the animation the smoke checks.

``make ui-e2e`` runs this against a scratch server, and
``tools/ui-smoke/live-topology.mjs`` relies on its ids and on the knobs it moves
every second. It is a test fixture: change it together with the smoke. The
examples to learn from are in ``examples/``.
"""

from __future__ import annotations

import asyncio
import math
import random
import time

from frequenz.quantities import Energy, Percentage, Power, Voltage

import macrocosim as mc

kW = Power.from_kilowatts
pct = Percentage.from_percent

# -----------------------------------------------------------------------------
# Topology
# -----------------------------------------------------------------------------

BATTERY = mc.make_battery(
    id=1000,
    capacity=Energy.from_kilowatt_hours(92),
    initial_soc=pct(85),
    soc_lower=pct(10),
    soc_upper=pct(90),
    voltage=Voltage.from_volts(800),
    rated_lower=kW(-30),
    rated_upper=kW(30),
)
BATTERY_INVERTER = mc.make_battery_inverter(
    id=1001, rated_lower=kW(-30), rated_upper=kW(30), successors=[BATTERY]
)
SOLAR = mc.make_solar_inverter(
    id=200, rated_lower=kW(-30), rated_upper=kW(0), sunlight=pct(100)
)
EV_CHARGER = mc.make_ev_charger(id=1004, rated_lower=kW(0), rated_upper=kW(22))
CHP = mc.make_chp(id=1006)
# Left out of the gRPC component list and drawn dashed in the UI, but counted
# into the main meter.
CONSUMER = mc.make_meter(id=100, name="consumer", hidden=True)
MAIN_METER = mc.make_meter(
    id=2,
    successors=[
        mc.make_meter(id=1002, successors=[BATTERY_INVERTER]),
        mc.make_meter(id=1003, successors=[SOLAR]),
        mc.make_meter(id=1005, successors=[EV_CHARGER]),
        # The CHP produces 2 kW, measured at its meter.
        mc.make_meter(id=1007, power=kW(-2), successors=[CHP]),
        CONSUMER,
    ],
)
GRID = mc.make_grid_connection_point(
    id=1, rated_lower=kW(-90), rated_upper=kW(100), successors=[MAIN_METER]
)

MICROGRID = mc.make_microgrid(id=2200, name="Starter site", tso="TN", topology=GRID)


# -----------------------------------------------------------------------------
# Environment animation
# -----------------------------------------------------------------------------


def cloud_curve(t: float) -> Percentage:
    """Sunlight `t` seconds into a 10-minute cycle: 3 min at 80 %, a 2-min ramp
    down to 20 %, 2 min there, a 2-min ramp back up, then 80 % again."""
    if t < 180:
        return pct(80)
    if t < 300:
        return pct(80 - 0.5 * (t - 180))
    if t < 420:
        return pct(20)
    return pct(min(80, 20 + 0.5 * (t - 420)))


def consumer_load(t: float) -> Power:
    """The consumer load `t` seconds into a 15-minute cycle: a sine between 5
    and 30 kW, plus up to 500 W of noise either way."""
    watts = 17_500 + 12_500 * math.sin(2 * math.pi * t / 900)
    return Power.from_watts(watts + random.uniform(-500, 500))


async def _line_voltage(microgrid: mc.Microgrid) -> None:
    while True:
        await microgrid.set_voltage_per_phase(
            *(Voltage.from_volts(random.uniform(229, 231)) for _ in range(3))
        )
        await asyncio.sleep(0.2)


async def _sunlight(microgrid: mc.Microgrid) -> None:
    solar = microgrid[SOLAR]
    while True:
        await solar.sunlight.set(cloud_curve(time.time() % 600))
        await asyncio.sleep(1)


async def _consumer(microgrid: mc.Microgrid) -> None:
    consumer = microgrid[CONSUMER]
    while True:
        await consumer.power.set(consumer_load(time.time() % 900))
        await asyncio.sleep(1)


async def animate(microgrid: mc.Microgrid) -> None:
    """Animate the site until cancelled: each phase's line voltage between 229
    and 231 V every 0.2 s, and the sunlight and consumer load every second."""
    async with asyncio.TaskGroup() as tasks:
        tasks.create_task(_line_voltage(microgrid))
        tasks.create_task(_sunlight(microgrid))
        tasks.create_task(_consumer(microgrid))


async def plug_car(microgrid: mc.Microgrid) -> None:
    """Plug a car at 35 % into the charger. It idles until the app under test
    commands it."""
    try:
        await microgrid[EV_CHARGER].plug("sedan", soc=pct(35))
    except mc.Rejected as exc:
        # Any refusal is printed and the script goes on: on a re-run, the
        # charger still has its car and refuses another.
        print(f"plug('sedan') refused: {exc.text}")


async def main() -> None:
    async with mc.connect() as sim:
        plant = await sim.declare(MICROGRID)
        await plug_car(plant)
        await animate(plant)


if __name__ == "__main__":
    asyncio.run(main())
