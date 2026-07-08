"""Y1 gRPC transport with the Y3 builder: build a battery topology in
Python, then discover, command a setpoint, read it back, and watch the
bounds gateway reject an out-of-envelope command.

Talks to switchyard the same way a downstream control app would. Run with
the grpc extra installed (``pip install -e '.[grpc]'``):

    SWITCHYARD_BIN=../target/debug/switchyard python examples/component_setpoint.py
"""

from __future__ import annotations

import asyncio
from datetime import timedelta

from frequenz.quantities import Energy, Percentage, Power

import switchyard as sw

INVERTER = 3

TOPOLOGY = sw.Microgrid(
    id=1,
    topology=sw.grid(
        id=1,
        successors=[
            sw.meter(
                id=2,
                successors=[
                    sw.battery_inverter(
                        id=INVERTER,
                        rated=(Power.from_watts(-5000), Power.from_watts(5000)),
                        successors=[
                            sw.battery(
                                id=4,
                                capacity=Energy.from_kilowatt_hours(100),
                                soc=Percentage.from_percent(50),
                            )
                        ],
                    )
                ],
            )
        ],
    ),
)


async def main() -> None:
    with sw.launch(TOPOLOGY) as site:
        for c in site.components():
            print(f"  {c.category:20} id={c.id} name={c.name}")

        # Command a setpoint over gRPC and assert it settles.
        inv = site.component(INVERTER)
        inv.command(active_power=Power.from_kilowatts(2), lifetime=timedelta(seconds=60))
        await inv.expect.active_power(
            approx=Power.from_kilowatts(2),
            tol=Power.from_watts(300),
            timeout=timedelta(seconds=15),
        )
        await site.component(4).expect.soc(
            within=(Percentage.from_percent(49), Percentage.from_percent(51))
        )  # battery still ~50 %
        print("OK  setpoint settled at ~2 kW; battery SoC ~50 %")

        # Narrow the envelope; an out-of-envelope command must hard-error
        # (exactly as the production API gateway gates it).
        inv.command(bounds=(Power.from_kilowatts(-1), Power.from_kilowatts(1)))
        try:
            inv.command(active_power=Power.from_kilowatts(4))
        except sw.SetpointRejected as exc:
            print(f"OK  out-of-envelope setpoint rejected: {exc}"[:80])
        else:
            raise AssertionError("out-of-envelope setpoint was NOT rejected")


if __name__ == "__main__":
    asyncio.run(main())
