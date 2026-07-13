"""Python integration-testing client for the switchyard microgrid simulator.

Launch the simulator, build and drive a topology, inject faults, and assert
on the resulting grid state from inside pytest. See ``todo.org`` §Y in the
switchyard repo for the design and roadmap.

Physical quantities are
`frequenz-quantities <https://pypi.org/project/frequenz-quantities/>`_
(``Power``, ``Energy``, ``Percentage``, ``Frequency``) — import them from
there directly; times are :mod:`datetime`. :func:`to_lisp_atom` converts any
typed value to the Lisp literal switchyard reads.
"""

from __future__ import annotations

from . import aio
from .build import (
    Component,
    ConfigSource,
    LaunchConfig,
    LispRenderable,
    Microgrid,
    RawLisp,
    battery,
    battery_inverter,
    chp,
    ev_charger,
    grid,
    meter,
    raw,
    solar_inverter,
    to_lisp_atom,
)
from .enums import CommandMode, Health, Metric, Schedule, TelemetryMode
from .errors import (
    ControlRejected,
    EvalRejected,
    NoSample,
    SetpointRejected,
    SwitchyardError,
)
from .matchers import Matcher, at_least, at_most, between, near
from .metrics import MetricSpec
from .runtime import MicrogridEndpoint, Site, connect, launch
from .scenarios import (
    Check,
    JournalEvent,
    Scenario,
    ScenarioReport,
    ScenarioRun,
    run_scenario_stepped,
)
from .signals import CumulativeSignal, DrivenSignal, SettingSignal, Signal

__all__ = [
    # process + transport
    "Site",
    "MicrogridEndpoint",
    "launch",
    "connect",
    # async core
    "aio",
    # errors
    "SwitchyardError",
    "SetpointRejected",
    "EvalRejected",
    "ControlRejected",
    "NoSample",
    # signals + matchers
    "Signal",
    "CumulativeSignal",
    "DrivenSignal",
    "SettingSignal",
    "Matcher",
    "near",
    "between",
    "at_most",
    "at_least",
    # metric model
    "MetricSpec",
    # topology builder
    "Microgrid",
    "Component",
    "grid",
    "meter",
    "battery_inverter",
    "solar_inverter",
    "battery",
    "ev_charger",
    "chp",
    "raw",
    "RawLisp",
    "to_lisp_atom",
    "ConfigSource",
    "LaunchConfig",
    "LispRenderable",
    # typed knobs
    "Health",
    "TelemetryMode",
    "CommandMode",
    "Metric",
    "Schedule",
    # scenarios
    "Scenario",
    "Check",
    "ScenarioRun",
    "ScenarioReport",
    "JournalEvent",
    "run_scenario_stepped",
]
