"""Demo pytest suite driving switchyard through the plugin fixtures.

Run against a built binary:

    SWITCHYARD_BIN=../../target/debug/switchyard \
        python -m pytest examples/pytest_demo -q
"""

from __future__ import annotations

from datetime import timedelta

import pytest
from frequenz.quantities import Energy, Percentage, Power

import switchyard as sw


@pytest.fixture
def switchyard_config():
    return sw.Microgrid(
        id=1,
        topology=sw.grid(
            id=1,
            successors=[
                sw.meter(
                    id=2,
                    power=Power.from_watts(7000),
                    successors=[
                        sw.battery_inverter(
                            id=3,
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


async def test_grid_power_holds(switchyard: sw.Site) -> None:
    await switchyard.expect.grid_power(
        approx=Power.from_kilowatts(7),
        tol=Power.from_kilowatts(1),
        timeout=timedelta(seconds=15),
    )


async def test_setpoint_then_fault(switchyard: sw.Site) -> None:
    inv = switchyard.component(3)
    inv.command(active_power=Power.from_kilowatts(2), lifetime=timedelta(seconds=60))
    await inv.expect.active_power(
        approx=Power.from_kilowatts(2), tol=Power.from_watts(300)
    )
    inv.status(health=sw.Health.ERROR)
    await inv.expect.active_power(approx=Power.from_watts(0), tol=Power.from_watts(100))
