"""Recording a test run, with named events, and checking its report.

``async with microgrid.record(name)`` records the block: peak grid power,
battery charge and discharge, PV energy, SoC statistics and 15-minute averages
of the grid power. ``recording.event`` marks a moment with a kind and a payload.
With ``csv=True`` every component's telemetry also goes to CSV files under
``recordings/<microgrid id>/`` in the simulator's state directory, which the
``launch_microgrid`` fixture (conftest.py) keeps in pytest's ``tmp_path``.
"""

from __future__ import annotations

from collections.abc import Awaitable, Callable
from datetime import timedelta
from pathlib import Path

from frequenz.quantities import Power

import macrocosim as mc

LaunchMicrogrid = Callable[[mc.MicrogridSpec], Awaitable[mc.Microgrid]]

kW = Power.from_kilowatts

LOAD = mc.make_meter(id=2201, name="load")
MICROGRID = mc.make_microgrid(
    id=22,
    name="Recorded run",
    topology=mc.make_grid_connection_point(id=2200, successors=[LOAD]),
)


async def test_a_recording_measures_the_peak(
    launch_microgrid: LaunchMicrogrid, tmp_path: Path
) -> None:
    microgrid = await launch_microgrid(MICROGRID)
    load = microgrid[LOAD]
    await load.power.set(kW(10))
    await microgrid.grid_power.expect(mc.near(kW(10), tol=kW(1)))

    async with microgrid.record("load-step", csv=True) as recording:
        await load.power.set(kW(30))
        await recording.event("load", {"kw": 30})
        await microgrid.grid_power.expect(
            mc.near(kW(30), tol=kW(1)), hold_for=timedelta(seconds=5)
        )
        await load.power.set(kW(10))
        await microgrid.grid_power.expect(mc.near(kW(10), tol=kW(1)))

    report = recording.report()
    assert 29_000 <= report.peak_grid_w <= 31_000, report
    assert list(tmp_path.rglob("recordings/22/load-step-*/*.csv")), "CSV files"
