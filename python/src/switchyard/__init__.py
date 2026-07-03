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
from .errors import SetpointRejected, SwitchyardError

__all__ = [
    # errors
    "SwitchyardError",
    "SetpointRejected",
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
]
