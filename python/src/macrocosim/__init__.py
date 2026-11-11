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
    Breaker,
    Chp,
    Component,
    EvCharger,
    GridConnectionPoint,
    Meter,
    PowerTransformer,
    SolarInverter,
    SteamBoiler,
    WindTurbine,
)
from ._connect import Connection, connect
from ._enums import CommandMode, EvIdle, Health, OperationalMode, TelemetryMode
from ._errors import (
    ConnectionClosed,
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
from ._recording import Recording, RecordingReport
from ._signals import ClearableKnob, ClearableSignal, Knob, SettableSignal, Signal
from ._spec import (
    BatteryInverterSpec,
    BatterySpec,
    BreakerSpec,
    ChpSpec,
    ComponentSpec,
    EvChargerSpec,
    GridConnectionPointSpec,
    MeterSpec,
    MicrogridSpec,
    PowerTransformerSpec,
    SolarInverterSpec,
    SteamBoilerSpec,
    WindTurbineSpec,
    make_battery,
    make_battery_inverter,
    make_breaker,
    make_chp,
    make_ev_charger,
    make_grid_connection_point,
    make_meter,
    make_microgrid,
    make_power_transformer,
    make_solar_inverter,
    make_steam_boiler,
    make_wind_turbine,
)
from ._topology import load_topology
from ._units import MassFlow, Pressure

__all__ = [
    "launch", "connect", "Connection", "load_topology", "Microgrid", "Recording",
    "RecordingReport",
    "Component", "GridConnectionPoint", "Meter", "Battery",
    "BatteryInverter", "SolarInverter", "Chp", "EvCharger", "SteamBoiler",
    "WindTurbine", "PowerTransformer", "Breaker",
    "ComponentSpec", "MicrogridSpec", "GridConnectionPointSpec", "MeterSpec",
    "BatterySpec", "BatteryInverterSpec", "SolarInverterSpec", "ChpSpec",
    "EvChargerSpec", "SteamBoilerSpec", "WindTurbineSpec",
    "PowerTransformerSpec", "BreakerSpec",
    "make_microgrid", "make_grid_connection_point", "make_meter",
    "make_battery", "make_battery_inverter", "make_solar_inverter", "make_chp",
    "make_ev_charger", "make_steam_boiler", "make_wind_turbine",
    "make_power_transformer", "make_breaker",
    "Matcher", "near", "between", "at_most", "at_least",
    "Signal", "SettableSignal", "ClearableSignal", "Knob", "ClearableKnob",
    "Health", "TelemetryMode", "CommandMode", "OperationalMode", "EvIdle",
    "Pressure", "MassFlow",
    "MacrocosimError", "SpecError", "LaunchError", "ServerError",
    "SimulatorExited", "Rejected", "UnknownComponent", "NoSample",
    "MicrogridClosed", "ConnectionClosed", "ExpectationFailed",
]  # fmt: skip
