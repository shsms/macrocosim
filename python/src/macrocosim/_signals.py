"""Signals: the attributes of live components that a test reads or sets.

Internal; imports `_matchers`, `_sampling`, `_session`, `_units` and `_enums`. A
signal reads through a source (a component's history, or an aggregate stream of
the microgrid) and writes through the ``/drive`` and ``/status`` routes.
"""

from __future__ import annotations

from collections.abc import Sequence
from dataclasses import dataclass
from datetime import datetime, timedelta
from enum import StrEnum
from functools import partial
from pathlib import Path
from typing import Any, Generic, TypeVar

from frequenz.quantities import Energy, Percentage, Power, ReactivePower

from ._enums import CommandMode, Health, TelemetryMode
from ._matchers import Matcher
from ._sampling import (
    HOLD_MAX_AGGREGATE,
    HOLD_MAX_COMPONENT,
    REAL_CLOCK,
    Sample,
    _Clock,
    _Source,
    expect,
    read,
)
from ._session import _Session
from ._units import UNITS, check_finite, check_type, to_wire

Q = TypeVar("Q")
V = TypeVar("V")

READ_TIMEOUT = timedelta(seconds=5)
EXPECT_TIMEOUT = timedelta(seconds=10)


@dataclass(frozen=True)
class _Setter:
    """How a component value is written: the route it is posted to (``drive`` or
    ``status``), its field there, and the ``drive`` flag that clears it, if any."""

    route: str
    field: str
    clear_flag: str | None = None


@dataclass(frozen=True)
class _SignalDef:
    """A component signal: its type, history metric and, if settable, its
    setter."""

    kind: type
    metric: str
    setter: _Setter | None = None


@dataclass(frozen=True)
class _KnobDef:
    """A write-only setting: its type and setter."""

    kind: type
    setter: _Setter


@dataclass(frozen=True)
class _AggregateDef:
    """A microgrid aggregate and the loopback stream it reads."""

    name: str
    stream: str
    kind: type


POWER = _SignalDef(Power, "active_power_w", _Setter("drive", "power_w", "clear_power"))
REACTIVE = _SignalDef(
    ReactivePower,
    "reactive_power_var",
    _Setter("drive", "reactive_power_var", "clear_reactive"),
)
SOC = _SignalDef(Percentage, "soc_pct", _Setter("drive", "soc_pct"))
INVERTER_POWER = _SignalDef(Power, "active_power_w")
SUNLIGHT = _KnobDef(Percentage, _Setter("drive", "sunlight_pct", "clear_sunlight"))
HEALTH = _KnobDef(Health, _Setter("status", "health"))
TELEMETRY_MODE = _KnobDef(TelemetryMode, _Setter("status", "telemetry_mode"))
COMMAND_MODE = _KnobDef(CommandMode, _Setter("status", "command_mode"))

AGGREGATES: tuple[_AggregateDef, ...] = (
    _AggregateDef("grid_power", "grid_power", Power),
    _AggregateDef("pv_power", "pv_power", Power),
    _AggregateDef("consumer_power", "consumer_power", Power),
    _AggregateDef("battery_power", "battery_pool_power", Power),
    _AggregateDef("grid_energy", "grid_energy", Energy),
    _AggregateDef("pv_energy", "pv_energy", Energy),
    _AggregateDef("consumer_energy", "consumer_energy", Energy),
    _AggregateDef("battery_energy", "battery_pool_energy", Energy),
)
"""The eight aggregates. A category the topology lacks (no solar, say) reads
0, as the simulator publishes it."""


def _sample(t: Any, v: Any) -> Sample:
    """One JSON ``(t_s, value)`` pair as floats, with a null value kept."""
    return (float(t), None if v is None else float(v))


def _component_path(session: _Session, component_id: int, route: str) -> str:
    return session.microgrid_path(f"component/{component_id}/{route}")


class _SessionSource(Generic[Q]):
    """The base of `_ComponentHistorySource` and `_AggregateSource`: the session, the
    kept log its messages name, and the conversion from the wire."""

    def __init__(self, session: _Session, kind: type) -> None:
        self._session = session
        self.from_wire = UNITS[kind].from_wire

    @property
    def log_path(self) -> Path | None:
        return self._session.log_path


class _ComponentHistorySource(_SessionSource[Q]):
    """One metric of one component, from the simulator's 1 Hz sampler."""

    stale_after: float | None = None
    hold_max = HOLD_MAX_COMPONENT

    def __init__(
        self,
        session: _Session,
        component_id: int,
        metric: str,
        kind: type,
        label: str,
    ) -> None:
        super().__init__(session, kind)
        self._path = _component_path(session, component_id, "history")
        self._metric = metric
        self._label = label

    def describe(self) -> str:
        return self._label

    async def fetch(self, window_s: int) -> Sequence[Sample]:
        params = {"metric": self._metric, "window_s": window_s}
        return await self._session.get_parsed(
            self._path, _parse_component_history, params
        )

    async def fetch_hold(self, window_s: int) -> Sequence[Sample]:
        return await self.fetch(window_s)


def _epoch_s(ts: str) -> float:
    """An RFC 3339 timestamp (``...Z``) as epoch seconds."""
    return datetime.fromisoformat(ts).timestamp()


class _AggregateSource(_SessionSource[Q]):
    """One aggregate stream of the microgrid, from the simulator's loopback."""

    stale_after: float | None = 3.0
    hold_max = HOLD_MAX_AGGREGATE

    def __init__(self, session: _Session, definition: _AggregateDef) -> None:
        super().__init__(session, definition.kind)
        self._def = definition

    def describe(self) -> str:
        return f"microgrid.{self._def.name}"

    async def fetch(self, window_s: int) -> Sequence[Sample]:
        path = self._session.microgrid_path("metrics/latest")
        parse = partial(_parse_metrics_latest, stream=self._def.stream)
        return await self._session.get_parsed(path, parse)

    async def fetch_hold(self, window_s: int) -> Sequence[Sample]:
        path = self._session.microgrid_path("metrics/history")
        parse = partial(_parse_metrics_history, stream=self._def.stream)
        return await self._session.get_parsed(path, parse)


def _parse_component_history(body: Any) -> list[Sample]:
    """The samples of a component ``/history`` answer."""
    return [_sample(t, v) for t, v in body["samples"]]


def _parse_metrics_latest(body: Any, stream: str) -> list[Sample]:
    """The newest sample of `stream` in a ``/metrics/latest`` answer, as a list
    of one; empty if the answer has no such stream."""
    entry = body.get(stream)
    if entry is None:
        return []
    return [_sample(_epoch_s(entry["ts"]), entry.get("value"))]


def _parse_metrics_history(body: Any, stream: str) -> list[Sample]:
    """The samples of `stream` in a ``/metrics/history`` answer."""
    return [_sample(p["t_s"], p["value"]) for p in body.get(stream, [])]


def _signal_repr(self: Signal[Any]) -> str:
    return f"<{type(self).__name__} {self._source.describe()}>"


def _knob_repr(self: Knob[Any]) -> str:
    return f"<{type(self).__name__} {self._writer.label}>"


class Signal(Generic[Q]):
    """A measured value: read it, or wait for it to match.

    Can: `read`, `expect`. Cannot: set or clear.
    """

    def __init__(self, source: _Source[Q], *, clock: _Clock = REAL_CLOCK) -> None:
        self._source = source
        self._clock = clock

    __repr__ = _signal_repr

    async def read(self, *, timeout: timedelta = READ_TIMEOUT) -> Q:
        """The value of the first sample newer than the newest one the simulator
        had when this call began; samples with no value are skipped.

        Right after a `set`, this can still be the old value; use `expect` to
        wait for a change. Raises `NoSample` when none arrives in `timeout`.
        """
        return await read(self._source, timeout, clock=self._clock)

    async def expect(
        self,
        matcher: Matcher[Q],
        *,
        timeout: timedelta = EXPECT_TIMEOUT,
        hold_for: timedelta | None = None,
    ) -> None:
        """Wait up to `timeout` for a new sample to match; then, with
        `hold_for`, require every sample for that long to match too.

        The first matching sample starts the hold. Raises `ExpectationFailed`
        when samples disagree, and `NoSample` when they are missing.
        """
        await expect(self._source, matcher, timeout, hold_for, clock=self._clock)


class _Writer:
    """Posts one component attribute's sets and clears."""

    def __init__(
        self,
        session: _Session,
        component_id: int,
        label: str,
        kind: type,
        setter: _Setter,
    ) -> None:
        self._session = session
        self._component_id = component_id
        self.label = label
        """The attribute as messages name it (``Meter 11 power``)."""
        self._kind = kind
        self._setter = setter

    async def _post(self, route: str, body: dict[str, Any], call: str) -> None:
        await self._session.post_json(
            _component_path(self._session, self._component_id, route),
            body,
            call=f"{self.label}.{call}",
        )

    async def set(self, value: object) -> None:
        """Check `value` against the attribute's type, then post it."""
        check_type(self.label, value, self._kind)
        check_finite(self.label, value)
        wire = str(value) if isinstance(value, StrEnum) else to_wire(value)
        setter = self._setter
        await self._post(setter.route, {setter.field: wire}, f"set({value})")

    async def clear(self) -> None:
        """Post the ``drive`` flag that drops the set value."""
        assert self._setter.clear_flag is not None  # checked when the component builds it
        await self._post("drive", {self._setter.clear_flag: True}, "clear()")


class SettableSignal(Signal[Q]):
    """A measured value the test can also set.

    Can: `read`, `expect`, `set`. Cannot: clear.
    """

    def __init__(
        self, source: _Source[Q], writer: _Writer, *, clock: _Clock = REAL_CLOCK
    ) -> None:
        super().__init__(source, clock=clock)
        self._writer = writer

    async def set(self, value: Q) -> None:
        """Set the value; raises `Rejected` if the simulator refuses it."""
        await self._writer.set(value)


class ClearableSignal(SettableSignal[Q]):
    """A settable value that can also go back to what the simulator computes.

    Can: `read`, `expect`, `set`, `clear`.
    """

    async def clear(self) -> None:
        """Drop the set value (it does not restore the spec's value)."""
        await self._writer.clear()


class Knob(Generic[V]):
    """A write-only setting, such as a fault knob.

    Can: `set`. Cannot: read, expect or clear.
    """

    def __init__(self, writer: _Writer) -> None:
        self._writer = writer

    __repr__ = _knob_repr

    async def set(self, value: V) -> None:
        """Set the value; raises `Rejected` if the simulator refuses it."""
        await self._writer.set(value)


class ClearableKnob(Knob[V]):
    """A write-only setting that can go back to the simulator's own value (for
    sunlight: the site's weather).

    Can: `set`, `clear`. Cannot: read or expect.
    """

    async def clear(self) -> None:
        """Drop the set value."""
        await self._writer.clear()
