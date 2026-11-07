"""load_topology: a topology document back into specs."""

from __future__ import annotations

import json
from datetime import timedelta
from pathlib import Path
from typing import Any

import pytest
from frequenz.quantities import Current, Energy, Percentage, Power

import macrocosim as mc
from macrocosim._render import render
from macrocosim._spec import children
from macrocosim._topology import specs_from_doc

pytestmark = pytest.mark.filterwarnings("error")


def _every_category() -> mc.MicrogridSpec:
    battery = mc.make_battery(
        capacity=Energy.from_kilowatt_hours(10),
        initial_soc=Percentage.from_percent(42.5),
        health=mc.Health.STANDBY,
    )
    return mc.make_microgrid(
        id=7,
        name='it\'s "plant"',
        tso="TN",
        grpc_port=9100,
        physics_tick=timedelta(milliseconds=50),
        topology=mc.make_grid_connection_point(
            rated_fuse_current=Current.from_amperes(32),
            rated_upper=Power.from_kilowatts(30),
            successors=[
                mc.make_meter(
                    power=Power.from_kilowatts(5),
                    power_factor=0.9,
                    leading=True,
                    interval=timedelta(seconds=1),
                ),
                mc.make_battery_inverter(
                    successors=[battery], operational_mode=mc.OperationalMode.CONTROL_ONLY
                ),
                mc.make_battery_inverter(successors=[battery]),
                mc.make_solar_inverter(sunlight=Percentage.from_percent(80)),
                mc.make_chp(name="chp"),
                mc.make_ev_charger(phases=1, idle=mc.EvIdle.FULL),
                mc.make_steam_boiler(
                    target=mc.Pressure.from_bar(10),
                    demand=mc.MassFlow.from_kilograms_per_second(0.5),
                ),
                mc.make_wind_turbine(),
                mc.make_power_transformer(successors=[mc.make_meter()]),
                mc.make_breaker(
                    successors=[mc.make_meter(telemetry_mode=mc.TelemetryMode.SILENT)]
                ),
            ],
        ),
    )


def test_every_category_round_trips(tmp_path: Path) -> None:
    doc = render(_every_category())
    path = tmp_path / "topology.json"
    path.write_text(json.dumps(doc))
    [spec] = mc.load_topology(path)
    assert render(spec) == doc
    assert mc.load_topology(str(path))[0].name == 'it\'s "plant"'


def test_every_id_is_kept_explicit() -> None:
    [spec] = specs_from_doc(render(_every_category()))
    assert all(c.id is not None for c in spec._order)
    assert spec._ids == {c: c.id for c in spec._order}


def test_a_component_under_two_parents_is_one_spec() -> None:
    doc = {
        "schema_version": 1,
        "microgrids": [
            {
                "id": 3,
                "components": [
                    {"type": "battery", "id": 4},
                    {"type": "battery_inverter", "id": 2},
                    {"type": "battery_inverter", "id": 3},
                    {"type": "grid_connection_point", "id": 1},
                ],
                "connections": [[2, 4], [3, 4], [1, 2], [1, 3]],
            }
        ],
    }
    [spec] = specs_from_doc(doc)
    left, right = children(spec.topology)
    assert children(left)[0] is children(right)[0]
    assert render(spec) == doc


def _doc(components: list[dict[str, Any]], connections: list[list[int]]) -> Any:
    return {
        "schema_version": 1,
        "microgrids": [{"components": components, "connections": connections}],
    }


@pytest.mark.parametrize(
    ("components", "connections", "match"),
    [
        ([{"type": "meter", "id": 1}], [], "root must be exactly one grid connection"),
        (
            [
                {"type": "grid_connection_point", "id": 1},
                {"type": "grid_connection_point", "id": 2},
            ],
            [],
            "with no parent are: grid_connection_point 1, grid_connection_point 2",
        ),
        ([], [], "with no parent are: none"),
        (
            [
                {"type": "grid_connection_point", "id": 1},
                {"type": "meter", "id": 2},
                {"type": "meter", "id": 3},
            ],
            [[1, 2], [2, 3], [3, 2]],
            "is its own ancestor",
        ),
        (
            [{"type": "grid_connection_point", "id": 1}, {"type": "battery", "id": 2}],
            [[2, 1]],
            "with no parent are: battery 2",
        ),
        (
            [
                {"type": "grid_connection_point", "id": 1},
                {"type": "battery", "id": 2},
                {"type": "meter", "id": 3},
            ],
            [[1, 2], [2, 3]],
            "a battery \\(2\\) takes no children",
        ),
        (
            [{"type": "grid_connection_point", "id": 1}],
            [[1, 9]],
            "names component 9",
        ),
        (
            [{"type": "grid_connection_point", "id": 1}, {"type": "meter", "id": 1}],
            [],
            "component id 1 appears twice",
        ),
        ([{"type": "pump", "id": 1}], [], "unknown component type 'pump'"),
        ([{"type": "meter"}], [], "a meter has no id"),
        (
            [{"type": "grid_connection_point", "id": 1, "power_w": 3.0}],
            [],
            "has no field 'power_w'",
        ),
        (
            [{"type": "grid_connection_point", "id": 1, "health": "fine"}],
            [],
            "'fine' is not a Health",
        ),
        (
            [{"type": "grid_connection_point", "id": 1, "rated_upper_w": "big"}],
            [],
            "rated_upper_w must be float",
        ),
        (
            [{"type": "grid_connection_point", "id": 1, "rated_fuse_current_a": 1.5}],
            [],
            "whole amperes",
        ),
    ],
)
def test_a_bad_document_raises_spec_error(
    components: list[dict[str, Any]], connections: list[list[int]], match: str
) -> None:
    with pytest.raises(mc.SpecError, match=match):
        specs_from_doc(_doc(components, connections))


def test_what_the_simulator_accepts_is_read() -> None:
    [spec] = specs_from_doc(
        {
            "schema_version": 1,
            "microgrids": [
                {
                    "id": 1,
                    "physics_tick_s": 0,
                    "components": [
                        {
                            "type": "grid_connection_point",
                            "id": 1,
                            "operational_mode": "unspecified",
                        }
                    ],
                    "connections": [],
                }
            ],
        }
    )
    assert spec.physics_tick == timedelta(milliseconds=1)
    assert spec.topology.operational_mode is None


@pytest.mark.parametrize(
    ("microgrid", "component", "match"),
    [
        ({"physics_tick_s": -5}, {}, "physics_tick_s must not be negative"),
        ({}, {"health": "unspecified"}, "health"),
    ],
)
def test_what_the_simulator_refuses_raises_spec_error(
    microgrid: dict[str, Any], component: dict[str, Any], match: str
) -> None:
    doc = {
        "schema_version": 1,
        "microgrids": [
            {
                "id": 1,
                **microgrid,
                "components": [{"type": "grid_connection_point", "id": 1, **component}],
                "connections": [],
            }
        ],
    }
    with pytest.raises(mc.SpecError, match=match):
        specs_from_doc(doc)


def test_a_duration_out_of_range_raises_spec_error() -> None:
    doc = _doc(
        [
            {"type": "grid_connection_point", "id": 1},
            {"type": "meter", "id": 2, "interval_s": 1e15},
        ],
        [[1, 2]],
    )
    with pytest.raises(mc.SpecError, match="is not a duration"):
        specs_from_doc(doc)


def test_the_document_needs_its_schema_version() -> None:
    with pytest.raises(mc.SpecError, match="schema_version 2 is not supported"):
        specs_from_doc({"schema_version": 2, "microgrids": []})
    with pytest.raises(mc.SpecError, match="unknown keys \\['extra'\\]"):
        specs_from_doc({"schema_version": 1, "microgrids": [], "extra": 1})


def test_a_file_that_is_not_json_raises(tmp_path: Path) -> None:
    path = tmp_path / "t.json"
    path.write_text("{")
    with pytest.raises(mc.SpecError, match="is not JSON"):
        mc.load_topology(path)
