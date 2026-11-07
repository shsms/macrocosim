"""The exact topology document a microgrid spec renders to."""

from __future__ import annotations

import json
from datetime import timedelta
from enum import IntEnum
from typing import Any

import pytest
from frequenz.quantities import (
    Current,
    Energy,
    Percentage,
    Power,
    ReactivePower,
)

import macrocosim._spec as s
from macrocosim._enums import Health, OperationalMode, TelemetryMode
from macrocosim._render import render

pytestmark = pytest.mark.filterwarnings("error")


def _mg(*successors: s.ComponentSpec, **kw: object) -> s.MicrogridSpec:
    gcp_id = kw.pop("gcp_id", None)
    return s.make_microgrid(
        id=7,
        topology=s.make_grid_connection_point(id=gcp_id, successors=list(successors)),
        **kw,  # type: ignore[arg-type]
    )


def _one(spec: s.MicrogridSpec) -> dict[str, Any]:
    doc = render(spec)
    assert doc["schema_version"] == 1
    [microgrid] = doc["microgrids"]
    return microgrid


def _components(spec: s.MicrogridSpec) -> list[dict[str, Any]]:
    return _one(spec)["components"]


def test_a_chain() -> None:
    bat = s.make_battery(capacity=Energy.from_watt_hours(50000))
    mg = s.make_microgrid(
        id=7,
        name="north",
        tso="tso-a",
        topology=s.make_grid_connection_point(
            successors=[s.make_battery_inverter(successors=[bat])]
        ),
    )
    assert render(mg) == {
        "schema_version": 1,
        "microgrids": [
            {
                "id": 7,
                "name": "north",
                "tso": "tso-a",
                "components": [
                    {"type": "battery", "id": 1000, "capacity_wh": 50000.0},
                    {"type": "battery_inverter", "id": 1001},
                    {"type": "grid_connection_point", "id": 1002},
                ],
                "connections": [[1001, 1000], [1002, 1001]],
            }
        ],
    }


def test_a_diamond_lists_the_shared_inverter_once() -> None:
    inv = s.make_battery_inverter(successors=[s.make_battery()])
    mg = _mg(s.make_meter(successors=[inv]), s.make_meter(successors=[inv]))
    microgrid = _one(mg)
    assert [c["id"] for c in microgrid["components"]] == [1000, 1001, 1002, 1003, 1004]
    assert microgrid["connections"] == [
        [1001, 1000],
        [1002, 1001],
        [1003, 1001],
        [1004, 1002],
        [1004, 1003],
    ]


def test_a_shared_battery_under_two_inverters() -> None:
    bat = s.make_battery(id=30)
    mg = _mg(
        s.make_battery_inverter(id=10, successors=[bat]),
        s.make_battery_inverter(id=20, successors=[bat]),
        gcp_id=1,
    )
    microgrid = _one(mg)
    assert [c["id"] for c in microgrid["components"]] == [30, 10, 20, 1]
    assert microgrid["connections"] == [[10, 30], [20, 30], [1, 10], [1, 20]]


def test_partial_ids() -> None:
    mg = _mg(s.make_meter(), s.make_meter(id=1000), s.make_meter())
    assert [c["id"] for c in _components(mg)] == [1001, 1000, 1002, 1003]


def test_no_successors_gives_one_component_and_no_connections() -> None:
    mg = s.make_microgrid(topology=s.make_grid_connection_point(id=1, successors=[]))
    assert _one(mg) == {
        "components": [{"type": "grid_connection_point", "id": 1}],
        "connections": [],
    }


def test_the_microgrid_fields() -> None:
    mg = s.make_microgrid(
        id=7,
        grpc_port=9100,
        physics_tick=timedelta(milliseconds=250),
        topology=s.make_grid_connection_point(id=1),
    )
    microgrid = _one(mg)
    assert microgrid["grpc_port"] == 9100
    assert microgrid["physics_tick_s"] == 0.25


def test_integer_fields_have_no_decimal_point() -> None:
    mg = s.make_microgrid(
        id=7,
        topology=s.make_grid_connection_point(
            id=1, rated_fuse_current=Current.from_amperes(63)
        ),
    )
    [gcp] = _components(mg)
    assert gcp["rated_fuse_current_a"] == 63
    assert type(gcp["rated_fuse_current_a"]) is int


def test_an_intenum_id_renders_as_its_number() -> None:
    class Ids(IntEnum):
        MG = 3
        GRID = 1

    mg = s.make_microgrid(id=Ids.MG, topology=s.make_grid_connection_point(id=Ids.GRID))
    text = json.dumps(render(mg))
    assert '"id": 3' in text
    assert '{"type": "grid_connection_point", "id": 1}' in text


def test_value_kinds_render_correctly() -> None:
    m = s.make_meter(
        id=2,
        name="main",
        hidden=True,
        health=Health.ERROR,
        telemetry_mode=TelemetryMode.ERROR_EMPTY,
        operational_mode=OperationalMode.TELEMETRY_ONLY,
        interval=timedelta(seconds=5),
    )
    assert _components(_mg(m))[0] == {
        "type": "meter",
        "id": 2,
        "name": "main",
        "operational_mode": "telemetry_only",
        "health": "error",
        "telemetry_mode": "error_empty",
        "interval_s": 5.0,
        "hidden": True,
    }


def test_meter_reactive_fields_render_correctly() -> None:
    components = _components(
        _mg(
            s.make_meter(
                id=2, reactive_power=ReactivePower.from_volt_amperes_reactive(1500)
            ),
            s.make_meter(id=3, power_factor=0.8, leading=True),
        )
    )
    assert components[0] == {"type": "meter", "id": 2, "reactive_power_var": 1500.0}
    assert components[1] == {
        "type": "meter",
        "id": 3,
        "power_factor": 0.8,
        "leading": True,
    }


def test_typed_quantities_render_in_base_units() -> None:
    components = _components(
        _mg(
            s.make_battery_inverter(
                id=2,
                command_delay=timedelta(milliseconds=200),
                successors=[
                    s.make_battery(id=1, capacity=Energy.from_kilowatt_hours(50))
                ],
            ),
            s.make_solar_inverter(
                id=3,
                sunlight=Percentage.from_percent(80),
                rated_lower=Power.from_kilowatts(-10),
            ),
        )
    )
    by_id = {c["id"]: c for c in components}
    assert by_id[1]["capacity_wh"] == 50000.0
    assert by_id[2]["command_delay_s"] == 0.2
    assert by_id[3]["sunlight_pct"] == 80.0
    assert by_id[3]["rated_lower_w"] == -10000.0


def test_strings_are_plain_json_strings() -> None:
    mg = s.make_microgrid(name='a "b" \\ c', topology=s.make_grid_connection_point())
    assert json.loads(json.dumps(render(mg)))["microgrids"][0]["name"] == 'a "b" \\ c'
