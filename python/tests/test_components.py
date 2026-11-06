"""Live classes: each has exactly its category's members."""

from __future__ import annotations

import httpx
import pytest
from frequenz.quantities import Power

from macrocosim._components import (
    Battery,
    BatteryInverter,
    Chp,
    Component,
    GridConnectionPoint,
    Meter,
    SolarInverter,
)
from macrocosim._session import _Session
from macrocosim._signals import (
    SOC,
    ClearableKnob,
    ClearableSignal,
    Knob,
    SettableSignal,
    Signal,
)

pytestmark = pytest.mark.filterwarnings("error")

BASE = {"id", "health", "telemetry_mode", "command_mode"}


def _public(cls: type) -> set[str]:
    return {n for n in dir(cls) if not n.startswith("_")}


@pytest.mark.parametrize(
    ("cls", "extra"),
    [
        (GridConnectionPoint, set()),
        (Chp, set()),
        (Meter, {"power", "reactive_power"}),
        (Battery, {"soc"}),
        (BatteryInverter, {"power"}),
        (SolarInverter, {"power", "sunlight"}),
    ],
)
def test_each_class_has_exactly_its_members(cls: type, extra: set[str]) -> None:
    assert issubclass(cls, Component)
    assert _public(cls) == BASE | extra


def _session() -> _Session:
    return _Session(
        "http://sim",
        7,
        transport=httpx.MockTransport(lambda r: httpx.Response(204)),
        exit_probe=lambda: None,
    )


async def test_signal_kinds() -> None:
    session = _session()
    meter = Meter(session, 104)
    inverter = BatteryInverter(session, 102)
    solar = SolarInverter(session, 105)
    battery = Battery(session, 1000)
    assert type(meter.power) is ClearableSignal
    assert type(meter.reactive_power) is ClearableSignal
    assert type(battery.soc) is SettableSignal
    assert type(inverter.power) is Signal
    assert type(solar.power) is Signal
    assert type(solar.sunlight) is ClearableKnob
    assert type(meter.health) is Knob
    assert not hasattr(inverter.power, "set")
    assert not hasattr(solar.sunlight, "read")
    assert repr(inverter.power) == "<Signal BatteryInverter 102 power>"
    await session.close()


async def test_a_bare_number_to_set_is_a_type_error() -> None:
    session = _session()
    with pytest.raises(TypeError, match="power must be Power, not float"):
        await Meter(session, 104).power.set(5.0)  # type: ignore[arg-type]
    await Meter(session, 104).power.set(Power.from_watts(5))
    await session.close()


def test_a_clearable_signal_needs_a_clear_flag() -> None:
    meter = Meter(_session(), 104)
    with pytest.raises(TypeError, match="Meter 104 soc has no clear"):
        meter._settable(ClearableSignal, SOC, "soc")
