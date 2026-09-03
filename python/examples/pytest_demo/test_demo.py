"""Demo pytest suite driving macrocosim through the plugin fixtures.

Run against a built binary:

    MACROCOSIM_BIN=../../target/debug/macrocosim \
        python -m pytest examples/pytest_demo -q
"""

from __future__ import annotations

from datetime import timedelta

import pytest
from frequenz.quantities import Energy, Percentage, Power

import macrocosim as mc


@pytest.fixture
def macrocosim_config():
    return mc.Microgrid(
        id=1,
        topology=mc.grid(
            id=1,
            successors=[
                mc.meter(
                    id=2,
                    power=Power.from_watts(7000),
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
                        )
                    ],
                )
            ],
        ),
    )


async def test_grid_power_holds(macrocosim: mc.Site) -> None:
    await macrocosim.expect.grid_power(
        approx=Power.from_kilowatts(7),
        tol=Power.from_kilowatts(1),
        timeout=timedelta(seconds=15),
    )


async def test_setpoint_then_fault(macrocosim: mc.Site) -> None:
    inv = macrocosim.component(3)
    inv.command(active_power=Power.from_kilowatts(2), lifetime=timedelta(seconds=60))
    await inv.expect.active_power(
        approx=Power.from_kilowatts(2), tol=Power.from_watts(300)
    )
    inv.status(health=mc.Health.ERROR)
    await inv.expect.active_power(approx=Power.from_watts(0), tol=Power.from_watts(100))
