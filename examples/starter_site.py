"""The starter site: one microgrid (id 2200) with a battery, PV, an EV charger, a
CHP and a hidden consumer load, the environment that animates it, and seven
scenarios that drive it.

Run it from the repository root, with the venv that ``uv sync`` makes in
``python/``::

    cargo run --bin macrocosim -- --enterprise-id 1 \\
        --python python/.venv/bin/python examples/starter_site.py

Saving this file restarts ``run``. Every component has an explicit id, so the
battery keeps its state of charge and the charger its car across the restart
(see docs/python-scripts.md).

Each scenario is an async function that takes the microgrid, records itself and
returns the recording's report. To run one with the script, await it in ``run``
beside the animation and save. To run one from a terminal while the script
runs, start ``python -m asyncio`` in this directory with the same venv::

    import macrocosim as mc
    import starter_site

    async with mc.connect("http://127.0.0.1:8801") as microgrids:
        report = await starter_site.battery_degraded_fleet(microgrids[2200])

From a terminal, a scenario cannot pause the script's animation, which runs in
the script's process, so the two that drive the consumer load or the sunlight
(`peak_evening_load`, `pv_dropout`) are overwritten within a second; run those
with the script.
"""

from __future__ import annotations

import asyncio
import contextlib
import math
import random
import time
from collections.abc import Iterator
from typing import Any, TypeVar, cast

from frequenz.quantities import Energy, Frequency, Percentage, Power, Voltage

import macrocosim as mc

kW = Power.from_kilowatts
pct = Percentage.from_percent

C = TypeVar("C", bound=mc.Component)

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

MICROGRIDS = [
    mc.make_microgrid(
        id=2200, name="Starter site", tso="TN", grpc_port=8800, topology=GRID
    ),
]


def component(microgrid: mc.Microgrid, spec: mc.ComponentSpec[C]) -> C:
    """`spec`'s component in `microgrid`, found by its id.

    ``microgrid[spec]`` matches spec objects by identity, and a microgrid from
    `mc.connect` is built from the simulator's copy of the specs, so the
    scenarios look components up by id to work from a terminal too.
    """
    assert spec.id is not None
    return cast(C, microgrid[spec.id])


# -----------------------------------------------------------------------------
# Environment animation
# -----------------------------------------------------------------------------

HELD: set[mc.ComponentSpec[Any]] = set()
"""Components whose animated knob a scenario in this process has taken over."""


@contextlib.contextmanager
def holding(*specs: mc.ComponentSpec[Any]) -> Iterator[None]:
    """Pause the animation of `specs`' knobs for the block, so a scenario's own
    values stand; the animation takes them back afterwards."""
    HELD.update(specs)
    try:
        yield
    finally:
        HELD.difference_update(specs)


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
    solar = component(microgrid, SOLAR)
    while True:
        if SOLAR not in HELD:
            await solar.sunlight.set(cloud_curve(time.time() % 600))
        await asyncio.sleep(1)


async def _consumer(microgrid: mc.Microgrid) -> None:
    consumer = component(microgrid, CONSUMER)
    while True:
        if CONSUMER not in HELD:
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
        await component(microgrid, EV_CHARGER).plug("sedan", soc=pct(35))
    except mc.Rejected as exc:
        # After a reload the charger carries its car over, and refuses another.
        print(f"plug('sedan') refused: {exc.text}")


async def run(microgrids: dict[int, mc.Microgrid]) -> None:
    plant = microgrids[2200]
    await plug_car(plant)
    async with asyncio.TaskGroup() as tasks:
        tasks.create_task(animate(plant))
        # To run a scenario with the script, await it here and save, e.g.:
        # print(await pv_dropout(plant))


# -----------------------------------------------------------------------------
# Scenarios
# -----------------------------------------------------------------------------
# Each records itself and puts back in a `finally` every knob it changed, or
# hands it back to the animation. Setpoints come from the app under test over
# gRPC, so where the Lisp scenarios commanded the battery inverter, these leave
# it to the app.


async def peak_evening_load(microgrid: mc.Microgrid) -> mc.RecordingReport:
    """Consumer ramp, peak and wind-down with the PV gone; the battery has to
    discharge to cover. One step a minute, so it plays out in three."""
    consumer = component(microgrid, CONSUMER)
    solar = component(microgrid, SOLAR)
    with holding(CONSUMER, SOLAR):
        async with microgrid.record("peak-evening-load") as recording:
            await consumer.power.set(kW(12))
            await solar.sunlight.set(pct(10))
            await asyncio.sleep(60)
            await consumer.power.set(kW(25))
            await solar.sunlight.set(pct(0))
            await asyncio.sleep(60)
            await consumer.power.set(kW(6))
            await asyncio.sleep(60)
    return recording.report()


async def pv_dropout(microgrid: mc.Microgrid) -> mc.RecordingReport:
    """A cloud bank crosses the array: full sun, near-overcast, then clear, for
    checking that an app tracks the PV down and back up without overshooting."""
    solar = component(microgrid, SOLAR)
    with holding(SOLAR):
        async with microgrid.record("pv-dropout") as recording:
            await solar.sunlight.set(pct(80))
            await asyncio.sleep(60)
            await solar.sunlight.set(pct(15))
            await asyncio.sleep(60)
            await solar.sunlight.set(pct(80))
            await asyncio.sleep(60)
    return recording.report()


async def battery_degraded_fleet(microgrid: mc.Microgrid) -> mc.RecordingReport:
    """The battery flips between ok and error; the pool's bounds should shrink
    while it is out and recover when it is back."""
    battery = component(microgrid, BATTERY)
    try:
        async with microgrid.record("battery-degraded-fleet") as recording:
            for health in (mc.Health.OK, mc.Health.ERROR, mc.Health.OK):
                await battery.health.set(health)
                await asyncio.sleep(60)
            await battery.health.set(mc.Health.ERROR)
            await asyncio.sleep(60)
    finally:
        await battery.health.set(mc.Health.OK)
    return recording.report()


async def _restore_network(solar: mc.SolarInverter) -> None:
    # The simulator refuses a normal mode the component's operational mode or
    # health does not allow; the component then stays as those dictate.
    with contextlib.suppress(mc.Rejected):
        await solar.telemetry_mode.set(mc.TelemetryMode.NORMAL)
    with contextlib.suppress(mc.Rejected):
        await solar.command_mode.set(mc.CommandMode.NORMAL)


async def flaky_network(microgrid: mc.Microgrid) -> mc.RecordingReport:
    """The solar inverter stops streaming and its commands time out for a
    minute, while it keeps producing."""
    solar = component(microgrid, SOLAR)
    try:
        async with microgrid.record("flaky-network") as recording:
            await _restore_network(solar)
            await asyncio.sleep(60)
            await solar.telemetry_mode.set(mc.TelemetryMode.SILENT)
            await solar.command_mode.set(mc.CommandMode.TIMEOUT)
            await asyncio.sleep(60)
            await _restore_network(solar)
            await asyncio.sleep(60)
    finally:
        await _restore_network(solar)
    return recording.report()


async def frequency_deviation(microgrid: mc.Microgrid) -> mc.RecordingReport:
    """The grid frequency leans to 49.9 Hz, back to 50 and on to 50.1 Hz, a
    minute each, by moving the frequency model's nominal, so the noise stays
    on. The frequency is one value for the whole simulator."""
    hz = Frequency.from_hertz
    try:
        async with microgrid.record("frequency-deviation") as recording:
            for nominal in (49.9, 50.0, 50.1, 50.0):
                await microgrid.set_frequency_model(nominal=hz(nominal))
                await asyncio.sleep(60)
    finally:
        await microgrid.set_frequency_model(nominal=hz(50))
    return recording.report()


async def cold_start(microgrid: mc.Microgrid) -> mc.RecordingReport:
    """Every commandable component starts in standby and comes online in turn:
    the battery inverter, then the PV, then the EV charger and the CHP."""
    waves = [
        [component(microgrid, BATTERY_INVERTER)],
        [component(microgrid, SOLAR)],
        [component(microgrid, CHP), component(microgrid, EV_CHARGER)],
    ]
    try:
        async with microgrid.record("cold-start") as recording:
            for wave in waves:
                for part in wave:
                    await part.health.set(mc.Health.STANDBY)
            for wave in waves:
                await asyncio.sleep(60)
                for part in wave:
                    await part.health.set(mc.Health.OK)
            await asyncio.sleep(60)
    finally:
        for wave in waves:
            for part in wave:
                await part.health.set(mc.Health.OK)
    return recording.report()


async def off_grid_island(microgrid: mc.Microgrid) -> mc.RecordingReport:
    """The grid connection point fails for a minute, a forced island: the PV
    and the battery carry the load alone until it reconnects."""
    grid = component(microgrid, GRID)
    try:
        async with microgrid.record("off-grid-island") as recording:
            await grid.health.set(mc.Health.OK)
            await asyncio.sleep(60)
            await grid.health.set(mc.Health.ERROR)
            await asyncio.sleep(60)
            await grid.health.set(mc.Health.OK)
            await asyncio.sleep(60)
    finally:
        await grid.health.set(mc.Health.OK)
    return recording.report()
