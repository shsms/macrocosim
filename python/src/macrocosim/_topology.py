"""Read a topology document back into `MicrogridSpec` objects.

Internal; imports `_spec`, `_render`, `_units` and `_errors`. The document lists
each component once and the parent → child connections apart; this rebuilds the
nested specs, children first, so a component under two parents becomes one spec
object under both. Every component keeps the id the document gives it.
"""

from __future__ import annotations

import json
from datetime import timedelta
from enum import Enum
from pathlib import Path
from typing import Any

from ._enums import OperationalMode
from ._errors import SpecError
from ._render import SCHEMA_VERSION
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
    _field_kinds,
    _key_for_kind,
)
from ._units import UNITS, is_int_like

SPEC_CLASSES: dict[str, type[ComponentSpec[Any]]] = {
    cls._type: cls
    for cls in (
        GridConnectionPointSpec,
        MeterSpec,
        BatterySpec,
        BatteryInverterSpec,
        SolarInverterSpec,
        ChpSpec,
        EvChargerSpec,
        SteamBoilerSpec,
        WindTurbineSpec,
        PowerTransformerSpec,
        BreakerSpec,
    )
}
"""Each component spec class, by its ``type`` in the topology JSON."""

_MICROGRID_KEYS = {
    "id",
    "name",
    "tso",
    "grpc_port",
    "physics_tick_s",
    "components",
    "connections",
}


def load_topology(source: str | Path) -> list[MicrogridSpec]:
    """Read the topology document at the path `source`: one `MicrogridSpec` per
    microgrid, in the document's order.

    Every component keeps its id, and a component under two parents is one spec
    object under both. A document that is not a valid topology document, or a
    microgrid whose root is not exactly one grid connection point, raises
    `SpecError`.
    """
    path = Path(source)
    try:
        doc = json.loads(path.read_text())
    except json.JSONDecodeError as exc:
        raise SpecError(f"{path}: the topology document is not JSON: {exc}") from exc
    return specs_from_doc(doc)


def specs_from_doc(doc: Any) -> list[MicrogridSpec]:
    """The `MicrogridSpec` of each microgrid in a parsed topology document."""
    if not isinstance(doc, dict):
        raise SpecError("the topology document must be a JSON object")
    version = doc.get("schema_version")
    if version != SCHEMA_VERSION or isinstance(version, bool):
        raise SpecError(
            f"schema_version {version!r} is not supported; this client reads "
            f"schema_version {SCHEMA_VERSION}"
        )
    extra = set(doc) - {"schema_version", "microgrids"}
    if extra:
        raise SpecError(f"the topology document has unknown keys {sorted(extra)}")
    microgrids = doc.get("microgrids")
    if not isinstance(microgrids, list):
        raise SpecError("the topology document's microgrids must be a list")
    return [microgrid_from_value(mg) for mg in microgrids]


def microgrid_from_value(data: Any, label: str | None = None) -> MicrogridSpec:
    """One microgrid of a topology document as a `MicrogridSpec`. Errors name it
    by `label`, or by default as ``microgrid ID``."""
    if not isinstance(data, dict):
        raise SpecError("each microgrid must be a JSON object")
    if label is None:
        label = f"microgrid {data.get('id', '(no id)')}"
    extra = set(data) - _MICROGRID_KEYS
    if extra:
        raise SpecError(f"{label}: unknown keys {sorted(extra)}")
    components = data.get("components", [])
    connections = data.get("connections", [])
    if not isinstance(components, list) or not isinstance(connections, list):
        raise SpecError(f"{label}: components and connections must be lists")

    found: dict[int, tuple[type[ComponentSpec[Any]], dict[str, Any]]] = {}
    for component in components:
        cid, cls, fields = _component(label, component)
        if cid in found:
            raise SpecError(f"{label}: component id {cid} appears twice")
        found[cid] = (cls, fields)

    kids: dict[int, list[int]] = {cid: [] for cid in found}
    has_parent: set[int] = set()
    for pair in connections:
        if (
            not isinstance(pair, list)
            or len(pair) != 2
            or not all(is_int_like(x) for x in pair)
        ):
            raise SpecError(
                f"{label}: a connection must be [parent, child], not {pair!r}"
            )
        parent, child = pair
        for end in (parent, child):
            if end not in found:
                raise SpecError(
                    f"{label}: connection {pair!r} names component {end}, which is "
                    "not in the microgrid"
                )
        if child in kids[parent]:
            raise SpecError(f"{label}: connection {pair!r} appears twice")
        kids[parent].append(child)
        has_parent.add(child)

    roots = [cid for cid in found if cid not in has_parent]
    if len(roots) != 1 or found[roots[0]][0] is not GridConnectionPointSpec:
        shown = ", ".join(f"{found[r][0]._type} {r}" for r in roots) or "none"
        raise SpecError(
            f"{label}: the root must be exactly one grid connection point; the "
            f"components with no parent are: {shown}"
        )

    built: dict[int, ComponentSpec[Any]] = {}
    visiting: set[int] = set()

    def build(cid: int) -> ComponentSpec[Any]:
        spec = built.get(cid)
        if spec is not None:
            return spec
        if cid in visiting:
            raise SpecError(f"{label}: component {cid} is its own ancestor")
        visiting.add(cid)
        cls, fields = found[cid]
        successors = [build(k) for k in kids[cid]]
        if successors:
            if "successors" not in _field_kinds(cls):
                raise SpecError(f"{label}: a {cls._type} ({cid}) takes no children")
            fields = {**fields, "successors": successors}
        spec = _construct(f"{label} component {cid}", cls, {"id": cid, **fields})
        visiting.discard(cid)
        built[cid] = spec
        return spec

    topology = build(roots[0])
    if len(built) != len(found):
        stray = sorted(set(found) - set(built))
        raise SpecError(f"{label}: components {stray} are not under the root")
    assert isinstance(topology, GridConnectionPointSpec)

    kwargs: dict[str, Any] = {"topology": topology}
    for key in ("id", "grpc_port"):
        if key in data:
            kwargs[key] = _plain(f"{label} {key}", data[key], int)
    for key in ("name", "tso"):
        if key in data:
            kwargs[key] = _plain(f"{label} {key}", data[key], str)
    if "physics_tick_s" in data:
        tick = _plain(f"{label} physics_tick_s", data["physics_tick_s"], float)
        if tick < 0:
            raise SpecError(f"{label}: physics_tick_s must not be negative, got {tick}")
        # The simulator ticks in whole milliseconds, at least one.
        kwargs["physics_tick"] = _seconds(f"{label} physics_tick_s", max(tick, 0.001))
    return _construct(label, MicrogridSpec, kwargs)


def _component(
    label: str, data: Any
) -> tuple[int, type[ComponentSpec[Any]], dict[str, Any]]:
    """A component's id, spec class and field values, read from its JSON."""
    if not isinstance(data, dict):
        raise SpecError(f"{label}: each component must be a JSON object")
    kind = data.get("type")
    cls = SPEC_CLASSES.get(kind) if isinstance(kind, str) else None
    if cls is None:
        raise SpecError(f"{label}: unknown component type {kind!r}")
    if "id" not in data:
        raise SpecError(f"{label}: a {kind} has no id")
    cid = _plain(f"{label} {kind} id", data["id"], int)
    where = f"{label} component {cid}"
    by_key = {
        _key_for_kind(name, k): (name, k)
        for name, k in _field_kinds(cls).items()
        if name not in ("id", "successors")
    }
    fields: dict[str, Any] = {}
    for key, value in data.items():
        if key in ("type", "id"):
            continue
        if key not in by_key:
            raise SpecError(f"{where}: a {kind} has no field {key!r}")
        name, field_kind = by_key[key]
        if value == "unspecified" and field_kind is OperationalMode:
            # The simulator's default operational mode, which a spec leaves
            # unset.
            continue
        fields[name] = _from_wire(f"{where} {key}", value, field_kind)
    return cid, cls, fields


def _is_enum(kind: Any) -> bool:
    return isinstance(kind, type) and issubclass(kind, Enum)


def _seconds(label: str, seconds: float) -> timedelta:
    try:
        return timedelta(seconds=seconds)
    except (OverflowError, ValueError) as exc:
        raise SpecError(f"{label}: {seconds!r} s is not a duration: {exc}") from exc


def _from_wire(label: str, value: Any, kind: Any) -> Any:
    """A field's spec value from its wire value."""
    unit = UNITS.get(kind) if isinstance(kind, type) else None
    if unit is not None:
        number = _plain(label, value, float)
        if kind is timedelta:
            return _seconds(label, number)
        return unit.from_wire(number)
    if _is_enum(kind):
        if isinstance(value, str):
            for member in kind:
                if member.value == value:
                    return member
        raise SpecError(f"{label}: {value!r} is not a {kind.__name__}")
    if kind in (bool, str, int, float):
        return _plain(label, value, kind)
    raise SpecError(f"{label}: cannot read a field of type {kind!r}")


def _plain(label: str, value: Any, kind: type) -> Any:
    """`value` checked to be a JSON `kind`; an int for a `float` becomes one."""
    if kind is float and is_int_like(value):
        return float(value)
    if kind is int and is_int_like(value):
        return value
    if kind not in (int, float) and isinstance(value, kind):
        return value
    if kind is float and isinstance(value, float):
        return value
    raise SpecError(f"{label} must be {kind.__name__}, not {value!r}")


def _construct(label: str, cls: Any, kwargs: dict[str, Any]) -> Any:
    try:
        return cls(**kwargs)
    except SpecError:
        raise
    except (TypeError, ValueError) as exc:
        raise SpecError(f"{label}: {exc}") from exc
