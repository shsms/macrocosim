"""Enumerations for a component's configuration and runtime fault knobs.

Internal; imports nothing from the package. Each member's value is the exact
Lisp symbol and HTTP token the simulator expects (``src/sim/runtime.rs``,
``src/sim/component.rs``).
"""

from __future__ import annotations

from enum import StrEnum


class Health(StrEnum):
    """A component's reported health."""

    OK = "ok"
    ERROR = "error"
    STANDBY = "standby"


class TelemetryMode(StrEnum):
    """How a component's telemetry stream to the app behaves."""

    NORMAL = "normal"
    SILENT = "silent"
    CLOSED = "closed"
    ERROR_EMPTY = "error-empty"
    NOT_FOUND = "not-found"


class CommandMode(StrEnum):
    """How a component's control channel answers the app's commands."""

    NORMAL = "normal"
    TIMEOUT = "timeout"
    ERROR = "error"
    OVER_BOUND = "over-bound"


class OperationalMode(StrEnum):
    """What a component is configured to offer: telemetry, control, both, or
    neither.

    The simulator's ``unspecified`` default is not a member; leave the field
    unset (``None``) for it.
    """

    INACTIVE = "inactive"
    TELEMETRY_ONLY = "telemetry-only"
    CONTROL_ONLY = "control-only"
    CONTROL_AND_TELEMETRY = "control-and-telemetry"
