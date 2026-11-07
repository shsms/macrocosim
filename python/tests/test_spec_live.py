"""Spec field names against the simulator's JSON Schema.

The boots of rendered trees are in ``test_live.py``.
"""

from __future__ import annotations

from collections.abc import AsyncIterator
from typing import Any

import pytest
import pytest_asyncio

import macrocosim as mc
import macrocosim._spec as s

pytestmark = [pytest.mark.live, pytest.mark.filterwarnings("error")]

COMPONENT_SPECS = [
    s.GridConnectionPointSpec,
    s.MeterSpec,
    s.BatterySpec,
    s.BatteryInverterSpec,
    s.SolarInverterSpec,
    s.ChpSpec,
    s.EvChargerSpec,
    s.SteamBoilerSpec,
    s.WindTurbineSpec,
    s.PowerTransformerSpec,
    s.BreakerSpec,
]

MICROGRID_TREE = {"topology": {"components", "connections"}}
"""The microgrid spec's tree, which the document carries as two lists."""


@pytest_asyncio.fixture(scope="module", loop_scope="module")
async def schema() -> AsyncIterator[dict[str, Any]]:
    """The topology document's JSON Schema, from one simulator for the module."""
    spec = mc.make_microgrid(
        id=7,
        topology=mc.make_grid_connection_point(id=1, successors=[mc.make_meter(id=2)]),
    )
    async with mc.launch(spec) as microgrid:
        yield await microgrid._session.get_json_object("/api/spec/schema")


def _objects(schema: Any) -> list[dict[str, Any]]:
    """Every object schema in `schema` that lists its properties."""
    found, stack = [], [schema]
    while stack:
        value = stack.pop()
        if isinstance(value, dict):
            if "properties" in value:
                found.append(value)
            stack.extend(value.values())
        elif isinstance(value, list):
            stack.extend(value)
    return found


def _server_fields(schema: dict[str, Any], tag: str) -> set[str]:
    [component] = [
        o for o in _objects(schema) if o["properties"].get("type", {}).get("const") == tag
    ]
    return set(component["properties"]) - {"type"}


def _spec_fields(cls: type) -> set[str]:
    return {s.json_key(cls, name) for name in s._field_kinds(cls) if name != "successors"}


@pytest.mark.parametrize("cls", COMPONENT_SPECS, ids=lambda c: c.__name__)
async def test_spec_fields_match_the_server(cls: type, schema: dict[str, Any]) -> None:
    assert _spec_fields(cls) == _server_fields(schema, cls._type)  # type: ignore[attr-defined]


async def test_microgrid_fields_match_the_server(schema: dict[str, Any]) -> None:
    [microgrid] = [
        o
        for o in _objects(schema)
        if {"components", "connections"} <= set(o["properties"])
    ]
    ours: set[str] = set()
    for name in s._field_kinds(s.MicrogridSpec):
        ours |= MICROGRID_TREE.get(name, {s.json_key(s.MicrogridSpec, name)})
    assert ours == set(microgrid["properties"])


ENUMS = [mc.Health, mc.TelemetryMode, mc.CommandMode, mc.OperationalMode, mc.EvIdle]

SERVER_ONLY = {"OperationalMode": {"unspecified"}}
"""Values the server takes that the client reads as unset."""


def _enum_values(schema: dict[str, Any], name: str) -> set[str]:
    """The string values the schema's definition `name` allows."""
    defs = schema.get("$defs", {})
    assert name in defs, f"no {name} in the schema's $defs: {sorted(defs)}"
    found, stack = set(), [defs[name]]
    while stack:
        value = stack.pop()
        if isinstance(value, dict):
            if "enum" in value:
                found |= {v for v in value["enum"] if isinstance(v, str)}
            if isinstance(value.get("const"), str):
                found.add(value["const"])
            stack.extend(v for k, v in value.items() if k in ("oneOf", "anyOf"))
        elif isinstance(value, list):
            stack.extend(value)
    return found


@pytest.mark.parametrize("enum", ENUMS, ids=lambda e: e.__name__)
async def test_enum_values_match_the_server(enum: type, schema: dict[str, Any]) -> None:
    ours = {m.value for m in enum}  # type: ignore[attr-defined]
    server = _enum_values(schema, enum.__name__)
    assert ours == server - SERVER_ONLY.get(enum.__name__, set())
