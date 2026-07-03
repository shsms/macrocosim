"""Pythonic topology builder that emits switchyard's ``(make-*)`` Lisp.

The spec *is* the graph: nest children with ``successors=[…]``, exactly as
``(make-grid-connection-point … :successors (list (make-meter …)))`` in a
hand-written config. Each constructor returns a :class:`Component` node;
:meth:`Microgrid.to_lisp` walks the tree and renders the nested form 1:1.

Values are strongly typed — physical quantities are
`frequenz-quantities <https://pypi.org/project/frequenz-quantities/>`_
(``Power``, ``Energy``, ``Percentage``, …), times are :mod:`datetime`, and
runtime knobs are the :mod:`switchyard.enums` enums. :func:`to_lisp_atom`
converts any of these to the Lisp-supported literal switchyard reads;
:func:`raw` is the one escape hatch for splicing a literal form.
"""

from __future__ import annotations

import os
from collections.abc import Mapping, Sequence
from dataclasses import dataclass, field
from datetime import time, timedelta
from enum import StrEnum
from pathlib import Path
from typing import Protocol, TypeAlias, runtime_checkable

from frequenz.quantities import Energy, Percentage, Power, Quantity

from .enums import CommandMode, Health, TelemetryMode


@dataclass(frozen=True)
class RawLisp:
    """A literal Lisp form spliced verbatim into the emitted config."""

    text: str


def raw(text: str) -> RawLisp:
    """Splice a literal Lisp form (a value, or a whole ``(make-* …)``)."""
    return RawLisp(text)


# A value that can appear in an emitted plist: a Lisp scalar, a typed physical
# quantity, a datetime, an enum symbol, or a spliced raw form.
Value: TypeAlias = (
    bool | int | float | str | Quantity | timedelta | time | StrEnum | RawLisp
)
# A microgrid's topology: one root, a raw form, a sequence of them, or none.
Topology: TypeAlias = "Component | RawLisp | Sequence[Component | RawLisp] | None"


@runtime_checkable
class LispRenderable(Protocol):
    """Anything that renders itself to a Lisp config form (a ``Microgrid``)."""

    def to_lisp(self) -> str: ...


# A single config to launch: a ``.lisp`` path or one renderable (a ``Microgrid``).
LaunchConfig: TypeAlias = "str | os.PathLike[str] | LispRenderable"
# A config to render: a ``LaunchConfig`` or a sequence of renderables (e.g. a
# ``Microgrid`` plus ``Scenario`` s), for the stepped headless runner.
ConfigSource: TypeAlias = "LaunchConfig | Sequence[LispRenderable]"


def _lisp_string(value: str) -> str:
    escaped = value.replace("\\", "\\\\").replace('"', '\\"')
    return f'"{escaped}"'


def to_lisp_atom(value: Value) -> str:
    """Render a typed Python value as the Lisp literal switchyard reads.

    Physical quantities collapse to their SI base unit (``Power`` → watts,
    ``Energy`` → watt-hours, ``Percentage`` → percent, ``Frequency`` → hertz),
    a ``timedelta`` to seconds, a ``datetime.time`` to an ``"HH:MM:SS"`` string,
    an enum to its ``'symbol``, and ``bool`` to ``t`` / ``nil``.
    """
    if isinstance(value, RawLisp):
        return value.text
    if isinstance(value, bool):
        return "t" if value else "nil"
    if isinstance(value, Quantity):
        return repr(value.base_value)
    if isinstance(value, timedelta):
        return repr(value.total_seconds())
    if isinstance(value, time):
        return _lisp_string(value.isoformat())
    if isinstance(value, StrEnum):
        return f"'{value}"
    if isinstance(value, int):
        return str(value)
    if isinstance(value, float):
        return repr(value)
    return _lisp_string(value)


def _normalize(kwargs: Mapping[str, Value | None]) -> dict[str, Value]:
    """Resolve constructor kwargs to a plist-key → value dict.

    Only the convenience *key* renames live here; value conversion is
    :func:`to_lisp_atom`'s job, at emit time.
    """
    renames = {"soc": "initial-soc", "sunlight": "sunlight%"}
    out: dict[str, Value] = {}
    for key, value in kwargs.items():
        if value is None:
            continue
        out[renames.get(key, key.replace("_", "-"))] = value
    return out


@dataclass
class Component:
    """One node in the topology tree — a ``(make-*)`` form and its children."""

    make: str
    args: dict[str, Value] = field(default_factory=dict)
    successors: list[Component] = field(default_factory=list)

    def to_lisp(self) -> str:
        parts = [f":{key} {to_lisp_atom(val)}" for key, val in self.args.items()]
        if self.successors:
            children = " ".join(c.to_lisp() for c in self.successors)
            parts.append(f":successors (list {children})")
        body = (" " + " ".join(parts)) if parts else ""
        return f"({self.make}{body})"


def _component(
    make: str,
    args: Mapping[str, Value | None],
    *,
    rated: tuple[Power, Power] | None = None,
    successors: Sequence[Component] | None = None,
) -> Component:
    normalized = _normalize(args)
    if rated is not None:
        normalized["rated-lower"], normalized["rated-upper"] = rated
    return Component(make, normalized, list(successors or []))


# --- constructors (parents take successors; leaves don't) -----------------


def grid(
    *,
    id: int | None = None,
    name: str | None = None,
    rated: tuple[Power, Power] | None = None,
    health: Health | None = None,
    telemetry_mode: TelemetryMode | None = None,
    command_mode: CommandMode | None = None,
    successors: Sequence[Component] | None = None,
    **extra: Value,
) -> Component:
    """A grid connection point (the microgrid's boundary to the public grid)."""
    args = {
        "id": id,
        "name": name,
        "health": health,
        "telemetry_mode": telemetry_mode,
        "command_mode": command_mode,
        **extra,
    }
    return _component(
        "make-grid-connection-point", args, rated=rated, successors=successors
    )


def meter(
    *,
    id: int | None = None,
    name: str | None = None,
    power: Power | RawLisp | None = None,
    health: Health | None = None,
    telemetry_mode: TelemetryMode | None = None,
    command_mode: CommandMode | None = None,
    successors: Sequence[Component] | None = None,
    **extra: Value,
) -> Component:
    """A meter. ``power`` seeds its published load (a ``Power``, or ``raw(...)``)."""
    args = {
        "id": id,
        "name": name,
        "power": power,
        "health": health,
        "telemetry_mode": telemetry_mode,
        "command_mode": command_mode,
        **extra,
    }
    return _component("make-meter", args, successors=successors)


def battery_inverter(
    *,
    id: int | None = None,
    name: str | None = None,
    rated: tuple[Power, Power] | None = None,
    health: Health | None = None,
    telemetry_mode: TelemetryMode | None = None,
    command_mode: CommandMode | None = None,
    successors: Sequence[Component] | None = None,
    **extra: Value,
) -> Component:
    """A battery inverter; give it a ``battery`` child via ``successors``."""
    args = {
        "id": id,
        "name": name,
        "health": health,
        "telemetry_mode": telemetry_mode,
        "command_mode": command_mode,
        **extra,
    }
    return _component("make-battery-inverter", args, rated=rated, successors=successors)


def solar_inverter(
    *,
    id: int | None = None,
    name: str | None = None,
    rated: tuple[Power, Power] | None = None,
    sunlight: Percentage | None = None,
    health: Health | None = None,
    telemetry_mode: TelemetryMode | None = None,
    command_mode: CommandMode | None = None,
    successors: Sequence[Component] | None = None,
    **extra: Value,
) -> Component:
    """A solar (PV) inverter. ``sunlight`` seeds its irradiance (a ``Percentage``)."""
    args = {
        "id": id,
        "name": name,
        "sunlight": sunlight,
        "health": health,
        "telemetry_mode": telemetry_mode,
        "command_mode": command_mode,
        **extra,
    }
    return _component("make-solar-inverter", args, rated=rated, successors=successors)


def battery(
    *,
    id: int | None = None,
    name: str | None = None,
    capacity: Energy | None = None,
    soc: Percentage | None = None,
    **extra: Value,
) -> Component:
    """A battery (leaf). ``capacity`` is an ``Energy``; ``soc`` a ``Percentage``."""
    args = {"id": id, "name": name, "capacity": capacity, "soc": soc, **extra}
    return _component("make-battery", args)


def ev_charger(
    *,
    id: int | None = None,
    name: str | None = None,
    rated: tuple[Power, Power] | None = None,
    **extra: Value,
) -> Component:
    """An EV charger (leaf)."""
    args = {"id": id, "name": name, **extra}
    return _component("make-ev-charger", args, rated=rated)


def chp(
    *,
    id: int | None = None,
    name: str | None = None,
    rated: tuple[Power, Power] | None = None,
    **extra: Value,
) -> Component:
    """A combined heat-and-power unit (leaf)."""
    args = {"id": id, "name": name, **extra}
    return _component("make-chp", args, rated=rated)


def _emit_topology(topology: Topology) -> str:
    if topology is None:
        return "nil"
    if isinstance(topology, RawLisp):
        return topology.text
    if isinstance(topology, Component):
        return topology.to_lisp()
    # a sequence of roots / raw forms
    return " ".join(_emit_topology(t) for t in topology)


@dataclass
class Microgrid:
    """A microgrid topology that renders to a ``(make-microgrid …)`` form."""

    id: int
    name: str | None = None
    grpc_port: int | None = None
    tso: str | None = None
    topology: Topology = None

    def to_lisp(self) -> str:
        head = f":id {self.id}"
        if self.name is not None:
            head += f" :name {_lisp_string(self.name)}"
        if self.grpc_port is not None:
            head += f" :grpc-port {self.grpc_port}"
        if self.tso is not None:
            head += f" :tso {_lisp_string(self.tso)}"
        body = _emit_topology(self.topology)
        return f"(make-microgrid {head}\n :topology (lambda () {body}))\n"

    @classmethod
    def from_lisp_file(cls, path: str | Path) -> Path:
        """Use an existing ``.lisp`` config as-is (the escape hatch).

        Returns the path (not a ``Microgrid``) so ``launch()`` loads the file
        directly, preserving its directory for any relative ``(load …)``.
        """
        return Path(path)
