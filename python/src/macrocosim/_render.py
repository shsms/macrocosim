"""Render a `MicrogridSpec` as the topology document the simulator boots from.

Internal; imports `_spec` and `_units`. The document lists every component once,
children before parents and every id written out, then the parent → child
connections, so a spec shared by two parents is one component in two
connections. Fields left unset are left out, so the simulator's defaults apply.
"""

from __future__ import annotations

from typing import Any

from ._spec import MicrogridSpec, children, spec_wire_fields
from ._units import to_wire

SCHEMA_VERSION = 1
"""The ``schema_version`` of the topology document this client writes."""


def render(*specs: MicrogridSpec) -> dict[str, Any]:
    """The topology document, as JSON-ready data, holding `specs`."""
    return {
        "schema_version": SCHEMA_VERSION,
        "microgrids": [render_microgrid(spec) for spec in specs],
    }


def render_microgrid(spec: MicrogridSpec) -> dict[str, Any]:
    """One microgrid of the topology document, as JSON-ready data."""
    ids = spec._ids
    microgrid: dict[str, Any] = {}
    if spec.id is not None:
        microgrid["id"] = int(spec.id)
    if spec.name is not None:
        microgrid["name"] = spec.name
    if spec.tso is not None:
        microgrid["tso"] = spec.tso
    if spec.grpc_port is not None:
        microgrid["grpc_port"] = int(spec.grpc_port)
    if spec.physics_tick is not None:
        microgrid["physics_tick_s"] = to_wire(spec.physics_tick)
    microgrid["components"] = [
        {"type": c._type, "id": ids[c], **dict(spec_wire_fields(c))} for c in spec._order
    ]
    microgrid["connections"] = [
        [ids[parent], ids[child]] for parent in spec._order for child in children(parent)
    ]
    return microgrid
