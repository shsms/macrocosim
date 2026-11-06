"""Integration-testing client for the macrocosim microgrid simulator.

Describe a microgrid with the ``make_*`` constructors, start a private simulator
with `launch`, point the app under test at ``microgrid.grpc_url``, then change
the simulated world and `expect` what it measures. Quantities are
`frequenz-quantities <https://pypi.org/project/frequenz-quantities/>`_ types;
durations are `datetime.timedelta`.
"""

from ._components import (
    Battery,
    BatteryInverter,
    Chp,
    Component,
    GridConnectionPoint,
    Meter,
    SolarInverter,
)
from ._enums import CommandMode, Health, OperationalMode, TelemetryMode
from ._errors import (
    ExpectationFailed,
    LaunchError,
    MacrocosimError,
    MicrogridClosed,
    NoSample,
    Rejected,
    ServerError,
    SimulatorExited,
    SpecError,
    UnknownComponent,
)
from ._launch import launch
from ._matchers import Matcher, at_least, at_most, between, near
from ._microgrid import Microgrid
from ._signals import ClearableKnob, ClearableSignal, Knob, SettableSignal, Signal
from ._spec import (
    BatteryInverterSpec,
    BatterySpec,
    ChpSpec,
    ComponentSpec,
    GridConnectionPointSpec,
    MeterSpec,
    MicrogridSpec,
    SolarInverterSpec,
    make_battery,
    make_battery_inverter,
    make_chp,
    make_grid_connection_point,
    make_meter,
    make_microgrid,
    make_solar_inverter,
)

__all__ = [
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
