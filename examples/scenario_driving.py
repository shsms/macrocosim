"""A 30-minute recorded stress run on the starter site (``starter_site.py``,
imported from this directory).

Start the simulator, then run this file against it in place of the starter site,
from the repository root::

    cargo run --bin macrocosim -- --python python/.venv/bin/python
    python/.venv/bin/python examples/scenario_driving.py

It declares the starter site's microgrid 2200 itself. When the starter site
script ran before, 2200 is unchanged and keeps its state.

It plugs the car in and starts the starter site's animation, then records 30
minutes under the name ``example-30min`` while it:

*   replaces the consumer load with a sharper profile: 5 kW, and a 25 kW spike
    in the last 100 seconds of every 15-minute window, the demand peak just
    before a billing window closes;
*   silences the solar inverter at minute 5: it keeps producing, but its
    telemetry stops and its commands time out;
*   covers the array with cloud from minute 10 to minute 15;
*   knocks the battery out for 1 to 3 minutes at a time, every 5 to 10 minutes.

Each change is a named event in the recording. Watch it in the UI's Report
panel, or read the recording's routes::

    curl -s http://127.0.0.1:8801/api/mg/2200/recording          # state
    curl -s http://127.0.0.1:8801/api/mg/2200/recording/events   # events
    curl -s http://127.0.0.1:8801/api/mg/2200/recording/report   # metrics

At the end it puts back every knob it changed, prints the report, and leaves the
starter site's animation running until the script is stopped.
"""

from __future__ import annotations

import asyncio
import contextlib
import random
import time

from frequenz.quantities import Percentage, Power

import macrocosim as mc
from starter_site import BATTERY, CONSUMER, MICROGRID, SOLAR, animate, holding, plug_car

kW = Power.from_kilowatts
pct = Percentage.from_percent

DURATION_S = 30 * 60
CSV = False
"""True also writes every component's telemetry to CSV files under
``recordings/2200/`` in the state directory."""


async def _spiky_load(microgrid: mc.Microgrid) -> None:
    consumer = microgrid[CONSUMER]
    while True:
        spike = kW(25) if time.time() % 900 > 800 else kW(0)
        await consumer.power.set(kW(5) + spike)
        await asyncio.sleep(1)


async def _silent_solar(microgrid: mc.Microgrid, recording: mc.Recording) -> None:
    solar = microgrid[SOLAR]
    await asyncio.sleep(5 * 60)
    await solar.telemetry_mode.set(mc.TelemetryMode.SILENT)
    await solar.command_mode.set(mc.CommandMode.TIMEOUT)
    await recording.event("silent", f"solar {SOLAR.id} stopped streaming")


async def _cloud(microgrid: mc.Microgrid, recording: mc.Recording) -> None:
    solar = microgrid[SOLAR]
    await asyncio.sleep(10 * 60)
    await solar.sunlight.set(pct(30))
    await recording.event("cloud", "covered until minute 15")
    await asyncio.sleep(5 * 60)
    await solar.sunlight.set(pct(100))
    await recording.event("cloud", "cleared")


async def _battery_outages(microgrid: mc.Microgrid, recording: mc.Recording) -> None:
    battery = microgrid[BATTERY]
    while True:
        await asyncio.sleep(random.uniform(5 * 60, 10 * 60))
        await battery.health.set(mc.Health.ERROR)
        await recording.event("outage", f"battery {BATTERY.id} down")
        try:
            await asyncio.sleep(random.uniform(60, 3 * 60))
        finally:
            await battery.health.set(mc.Health.OK)
        await recording.event("outage", f"battery {BATTERY.id} back")


async def stress_run(microgrid: mc.Microgrid) -> mc.RecordingReport:
    """Record the 30-minute stress run on the starter site and return its
    report. The animation of the starter site, if it runs in this process,
    leaves the consumer load and the sunlight to the run."""
    solar = microgrid[SOLAR]
    battery = microgrid[BATTERY]
    with holding(CONSUMER, SOLAR):
        try:
            async with microgrid.record("example-30min", csv=CSV) as recording:
                await recording.event("note", "example-30min started")
                async with asyncio.TaskGroup() as tasks:
                    parts = [
                        tasks.create_task(_spiky_load(microgrid)),
                        tasks.create_task(_silent_solar(microgrid, recording)),
                        tasks.create_task(_cloud(microgrid, recording)),
                        tasks.create_task(_battery_outages(microgrid, recording)),
                    ]
                    await asyncio.sleep(DURATION_S)
                    for part in parts:
                        part.cancel()
        finally:
            await battery.health.set(mc.Health.OK)
            with contextlib.suppress(mc.Rejected):
                await solar.telemetry_mode.set(mc.TelemetryMode.NORMAL)
            with contextlib.suppress(mc.Rejected):
                await solar.command_mode.set(mc.CommandMode.NORMAL)
    return recording.report()


async def main() -> None:
    async with mc.connect() as sim:
        plant = await sim.declare(MICROGRID)
        await plug_car(plant)
        async with asyncio.TaskGroup() as tasks:
            tasks.create_task(animate(plant))
            report = await stress_run(plant)
            print(
                f"{report.name}: {report.elapsed_s:.0f} s, ended by {report.ended_by}; "
                f"peak grid {report.peak_grid_w / 1000:.1f} kW, battery charged "
                f"{report.total_battery_charged_wh / 1000:.1f} kWh and discharged "
                f"{report.total_battery_discharged_wh / 1000:.1f} kWh, PV "
                f"{report.total_pv_produced_wh / 1000:.1f} kWh"
            )


if __name__ == "__main__":
    asyncio.run(main())
