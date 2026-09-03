"""Y4/Y5 together: drive the environment, inject a fault, assert the result.

Build a topology in Python, then use the component handles
(``site.component(id).command`` / ``.status`` / ``.drive``) and settle-aware ``expect`` to
exercise it the way an integration test would.

    MACROCOSIM_BIN=../target/debug/macrocosim python examples/environment_and_faults.py
"""

from __future__ import annotations

import asyncio
from datetime import timedelta

from frequenz.quantities import Energy, Percentage, Power

import macrocosim as mc

TOPOLOGY = mc.Microgrid(
    id=1,
    topology=mc.grid(
        id=1,
        successors=[
            mc.meter(
                id=2,
                successors=[
                    mc.battery_inverter(
                        id=3,
                        rated=(Power.from_watts(-5000), Power.from_watts(5000)),
                        successors=[
                            mc.battery(
                                id=4,
                                capacity=Energy.from_kilowatt_hours(100),
                                initial_soc=Percentage.from_percent(50),
                            )
                        ],
                    ),
                    mc.solar_inverter(
                        id=5,
                        rated=(Power.from_watts(-5000), Power.from_watts(0)),
                        sunlight=Percentage.from_percent(80),
                    ),
                    mc.meter(id=6, power=Power.from_watts(1000)),  # a consumer meter
                ],
            )
        ],
    ),
)


async def main() -> None:
    with mc.launch(TOPOLOGY) as site:
        # Command a battery-inverter setpoint, then trip it offline.
        inv = site.component(3)
        inv.command(active_power=Power.from_kilowatts(2), lifetime=timedelta(seconds=60))
        await inv.expect.active_power(
            approx=Power.from_kilowatts(2), tol=Power.from_watts(300)
        )
        print("OK  inverter holding 2 kW")

        inv.status(health=mc.Health.ERROR)
        await inv.expect.active_power(
            approx=Power.from_watts(0), tol=Power.from_watts(100)
        )
        print("OK  fault injected — inverter tripped to ~0 W")

        # Drive the environment and assert the aggregates react.
        site.component(6).drive(power=Power.from_kilowatts(3))
        await site.component(6).expect.active_power(
            approx=Power.from_kilowatts(3), tol=Power.from_watts(300)
        )
        site.component(5).drive(sunlight=Percentage.from_percent(0))
        await site.expect.pv_power(approx=Power.from_watts(0), tol=Power.from_watts(200))
        print("OK  drove load to 3 kW and PV to 0")


if __name__ == "__main__":
    asyncio.run(main())
