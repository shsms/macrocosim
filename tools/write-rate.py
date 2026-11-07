"""Measure how many knob writes per second one simulator sustains.

Launches a microgrid of ten load meters and sets their power, round robin, from
one asyncio task as fast as the responses come back, for 60 s. Prints the writes
per second. The Python README states the result, rounded down to a multiple of
50, as the supported write rate.

Run it from ``python/``, against the binary the wheel would ship::

    cargo build --release --bin macrocosim
    MACROCOSIM_BIN=../target/release/macrocosim uv run python ../tools/write-rate.py
"""

from __future__ import annotations

import argparse
import asyncio
import time

from frequenz.quantities import Power

import macrocosim as mc

METERS = 10


async def measure(seconds: float) -> tuple[int, float]:
    """Write for `seconds`; return the writes made and the time they took."""
    meters = [mc.make_meter(id=10 + n) for n in range(METERS)]
    spec = mc.make_microgrid(
        id=1, topology=mc.make_grid_connection_point(id=1, successors=meters)
    )
    async with mc.launch(spec) as microgrid:
        live = [microgrid[meter] for meter in meters]
        # Warm the connection and the routes before timing.
        for meter in live:
            await meter.power.set(Power.from_watts(0))
        writes = 0
        start = time.perf_counter()
        deadline = start + seconds
        while time.perf_counter() < deadline:
            meter = live[writes % METERS]
            await meter.power.set(Power.from_watts(1000 + writes % 1000))
            writes += 1
        return writes, time.perf_counter() - start


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Measure the knob writes per second one simulator sustains."
    )
    parser.add_argument(
        "--seconds", type=float, default=60.0, help="how long to write (default 60)"
    )
    args = parser.parse_args()
    writes, elapsed = asyncio.run(measure(args.seconds))
    print(f"{writes} writes in {elapsed:.1f} s: {writes / elapsed:.0f} writes per second")


if __name__ == "__main__":
    main()
