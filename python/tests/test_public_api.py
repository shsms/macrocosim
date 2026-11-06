"""The public surface: exactly these names, all importable."""

from __future__ import annotations

import pytest

import macrocosim

pytestmark = pytest.mark.filterwarnings("error")

EXPECTED = [
    "launch", "Microgrid",
    "Component", "GridConnectionPoint", "Meter", "Battery",
    "BatteryInverter", "SolarInverter", "Chp",
    "ComponentSpec", "MicrogridSpec", "GridConnectionPointSpec", "MeterSpec",
    "BatterySpec", "BatteryInverterSpec", "SolarInverterSpec", "ChpSpec",
    "make_microgrid", "make_grid_connection_point", "make_meter",
    "make_battery", "make_battery_inverter", "make_solar_inverter", "make_chp",
    "Matcher", "near", "between", "at_most", "at_least",
    "Signal", "SettableSignal", "ClearableSignal", "Knob", "ClearableKnob",
    "Health", "TelemetryMode", "CommandMode", "OperationalMode",
    "MacrocosimError", "SpecError", "LaunchError", "ServerError",
    "SimulatorExited", "Rejected", "UnknownComponent", "NoSample",
    "MicrogridClosed", "ExpectationFailed",
]  # fmt: skip


def test_all_is_exactly_the_public_surface() -> None:
    assert macrocosim.__all__ == EXPECTED


@pytest.mark.parametrize("name", EXPECTED)
def test_every_name_imports(name: str) -> None:
    namespace: dict[str, object] = {}
    exec(f"from macrocosim import {name}", namespace)
    assert namespace[name] is getattr(macrocosim, name)


def test_no_0_1_names_remain() -> None:
    for old in ("aio", "Site", "grid", "meter", "raw", "Scenario", "connect"):
        assert not hasattr(macrocosim, old), old
