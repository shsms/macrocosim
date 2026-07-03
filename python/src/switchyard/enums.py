"""Typed enumerations for switchyard's runtime knobs and scenario checks.

Each member's *value* is the exact Lisp symbol switchyard expects. These are
:class:`enum.StrEnum` s, so a member renders to that symbol when spliced into
an emitted form (``f"'{Health.ERROR}"`` → ``'error``) and still compares equal
to the bare string, but the enum is the typed, discoverable way to pass one.

Values mirror switchyard's Rust ``FromStr`` impls (``src/sim/runtime.rs``) and
the scenario metric parser (``src/lisp/defuns/scenarios.rs``).
"""

from __future__ import annotations

from enum import StrEnum


class Health(StrEnum):
    """A component's reported health (``set-component-health``)."""

    OK = "ok"
    ERROR = "error"
    STANDBY = "standby"


class TelemetryMode(StrEnum):
    """How a component's telemetry stream behaves (``set-component-telemetry-mode``)."""

    NORMAL = "normal"
    SILENT = "silent"
    CLOSED = "closed"
    ERROR_EMPTY = "error-empty"
    NOT_FOUND = "not-found"


class CommandMode(StrEnum):
    """How a component's control channel responds (``set-component-command-mode``)."""

    NORMAL = "normal"
    TIMEOUT = "timeout"
    ERROR = "error"
    OVER_BOUND = "over-bound"


class Metric(StrEnum):
    """A metric a scenario ``(check …)`` can assert on."""

    ACTIVE_POWER = "active-power"
    REACTIVE_POWER = "reactive-power"
    DC_POWER = "dc-power"
    SOC = "soc"
    FREQUENCY = "frequency"
    ACTIVE_POWER_BOUNDS_LOWER = "active-power-bounds-lower"
    ACTIVE_POWER_BOUNDS_UPPER = "active-power-bounds-upper"
    REACTIVE_POWER_BOUNDS_LOWER = "reactive-power-bounds-lower"
    REACTIVE_POWER_BOUNDS_UPPER = "reactive-power-bounds-upper"


class Schedule(StrEnum):
    """A scenario's timeline interpretation (``:schedule``)."""

    RELATIVE = "relative"
    ABSOLUTE = "absolute"
