"""macrocosim.export: specs written as a Python module, and read back."""

from __future__ import annotations

import json
import os
import subprocess
import sys
from datetime import timedelta
from typing import Any

import pytest
from frequenz.quantities import (
    ApparentPower,
    Current,
    Energy,
    Percentage,
    Power,
    ReactivePower,
    Voltage,
)

import macrocosim as mc
from macrocosim._render import render
from macrocosim._topology import specs_from_doc
from macrocosim.export import to_python

pytestmark = pytest.mark.filterwarnings("error")


def _every_category() -> list[mc.MicrogridSpec]:
    battery = mc.make_battery(
        capacity=Energy.from_kilowatt_hours(10),
        initial_soc=Percentage.from_percent(42.5),
        voltage=Voltage.from_volts(800),
        health=mc.Health.STANDBY,
    )
    first = mc.make_microgrid(
        id=7,
        name="it's \"plant\"\n'''",
        tso="TN",
        grpc_port=9100,
        physics_tick=timedelta(milliseconds=50),
        topology=mc.make_grid_connection_point(
            rated_fuse_current=Current.from_amperes(32),
            rated_lower=Power.from_kilowatts(-30),
            successors=[
                mc.make_meter(
                    reactive_power=ReactivePower.from_volt_amperes_reactive(300),
                    hidden=True,
                    name="'); import os #",
                ),
                mc.make_battery_inverter(
                    successors=[battery],
                    reactive_apparent=ApparentPower.from_volt_amperes(5000),
                    ramp_rate_w_per_s=200,
                    command_mode=mc.CommandMode.OVER_BOUND,
                ),
                mc.make_battery_inverter(successors=[battery]),
                mc.make_solar_inverter(weather_lag=timedelta(seconds=30)),
                mc.make_chp(),
                mc.make_ev_charger(phases=3, resume_on_recovery=False),
                mc.make_steam_boiler(
                    max=mc.Pressure.from_bar(12),
                    demand=mc.MassFlow.from_kilograms_per_second(0.25),
                ),
                mc.make_wind_turbine(),
                mc.make_power_transformer(successors=[mc.make_meter()]),
                mc.make_breaker(successors=[mc.make_meter()]),
            ],
        ),
    )
    second = mc.make_microgrid(
        id=8,
        topology=mc.make_grid_connection_point(
            successors=[mc.make_meter(operational_mode=mc.OperationalMode.INACTIVE)]
        ),
    )
    return [first, second]


def _doc(specs: list[mc.MicrogridSpec]) -> dict[str, Any]:
    return render(*specs)


def _run(text: str) -> mc.MicrogridSpec:
    """The ``MICROGRID`` of the module `text`, imported (not run)."""
    namespace: dict[str, Any] = {"__name__": "exported"}
    exec(compile(text, "<export>", "exec"), namespace)
    return namespace["MICROGRID"]


def test_every_category_writes_and_reads_back() -> None:
    texts = []
    for spec in _every_category():
        doc = _doc([spec])
        text = to_python(specs_from_doc(doc))
        assert _doc([_run(text)]) == doc
        texts.append(text)
    # Strings are written with repr, so none breaks out of its literal.
    for name in ("it's \"plant\"\n'''", "'); import os #"):
        assert f"name={name!r}" in texts[0]
    assert "import os\n" not in texts[0]


def test_the_module_declares_its_microgrid_only_when_run() -> None:
    [first, _] = _every_category()
    text = to_python([first])
    assert text.startswith('"""A microgrid exported from macrocosim')
    assert "\nimport asyncio\n" in text
    assert text.endswith(
        "async def main() -> None:\n"
        "    async with mc.connect() as sim:\n"
        "        await sim.declare(MICROGRID)\n"
        "\n\n"
        'if __name__ == "__main__":\n'
        "    asyncio.run(main())\n"
    )
    # Importing it builds MICROGRID and defines main, and connects nowhere.
    namespace: dict[str, Any] = {"__name__": "exported"}
    exec(compile(text, "<export>", "exec"), namespace)
    assert callable(namespace["main"])


def test_the_export_writes_one_microgrid() -> None:
    with pytest.raises(mc.SpecError, match="one microgrid per file, not 2"):
        to_python(_every_category())


def test_a_component_under_two_parents_is_one_variable() -> None:
    [first, _] = _every_category()
    text = to_python([first])
    battery_id = next(i for s, i in first._ids.items() if s._type == "battery")
    assert text.count(f"battery_{battery_id} = mc.make_battery(") == 1
    assert text.count(f"battery_{battery_id},") == 2
    back = _run(text)
    left, right = (
        c for c in back.topology.successors or () if c._type == "battery_inverter"
    )
    assert left.successors == right.successors


def test_every_id_is_written() -> None:
    [first, _] = _every_category()
    text = to_python([first])
    for cid in first._ids.values():
        assert f"id={cid}," in text or f"id={cid})" in text or f"(id={cid}" in text


def test_the_module_runs_from_stdin_to_stdout() -> None:
    [first, _] = _every_category()
    doc = _doc([first])
    out = subprocess.run(
        [sys.executable, "-P", "-m", "macrocosim.export"],
        input=json.dumps(doc),
        capture_output=True,
        text=True,
        check=True,
    )
    assert _doc([_run(out.stdout)]) == doc


def test_the_module_reads_and_writes_utf8_whatever_the_locale() -> None:
    doc = {
        "schema_version": 1,
        "microgrids": [
            {
                "id": 7,
                "name": "Łódź",
                "components": [
                    {"id": 1, "type": "grid_connection_point", "name": "Zürich"}
                ],
                "connections": [],
            }
        ],
    }
    out = subprocess.run(
        [sys.executable, "-P", "-m", "macrocosim.export"],
        input=json.dumps(doc, ensure_ascii=False).encode(),
        capture_output=True,
        env={**os.environ, "PYTHONIOENCODING": "cp1252"},
        check=True,
    )
    text = out.stdout.decode()
    assert "Łódź" in text and "Zürich" in text


def test_a_bad_document_exits_1_with_the_reason() -> None:
    out = subprocess.run(
        [sys.executable, "-P", "-m", "macrocosim.export"],
        input='{"schema_version": 1, "microgrids": [{"components": []}]}',
        capture_output=True,
        text=True,
    )
    assert out.returncode == 1
    assert out.stdout == ""
    assert "the root must be exactly one grid connection point" in out.stderr


def _with_hybrid() -> dict[str, Any]:
    """A microgrid, grid 1 → meter 2, with a hybrid inverter commented out
    under the meter."""
    return {
        "schema_version": 1,
        "microgrids": [
            {
                "id": 7,
                "components": [
                    {"type": "grid_connection_point", "id": 1},
                    {"type": "meter", "id": 2},
                ],
                "connections": [[1, 2]],
            }
        ],
        "commented": [
            {
                "parent": 2,
                "kind": "hybrid_inverter",
                "components": [
                    {"type": "battery", "id": 31, "capacity_wh": 10000.0},
                    {
                        "type": "battery_inverter",
                        "id": 30,
                        "name": "hybrid 1",
                        "rated_lower_w": -5000.0,
                        "rated_upper_w": 5000.0,
                    },
                ],
                "connections": [[30, 31]],
            }
        ],
    }


def _export(doc: dict[str, Any]) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, "-P", "-m", "macrocosim.export"],
        input=json.dumps(doc),
        capture_output=True,
        text=True,
    )


def _uncommented(text: str, constructor: str) -> str:
    """`text` with its commented-out components put back, their root written
    with `constructor`."""
    lines = []
    for line in text.split("\n"):
        body = line.lstrip()
        indent = line[: len(line) - len(body)]
        if body.startswith("# Needs"):
            continue
        if body.startswith("# "):
            line = indent + body[2:].replace("mc.make_hybrid_inverter(", constructor)
        lines.append(line)
    return "\n".join(lines)


def test_commented_components_are_written_in_their_parents_successors() -> None:
    out = _export(_with_hybrid())
    assert out.returncode == 0, out.stderr
    assert "        # Needs a hybrid inverter model:\n" in out.stdout
    assert "        # mc.make_hybrid_inverter(\n" in out.stdout
    assert "from frequenz.quantities import Energy, Power" in out.stdout
    # Commented out, they are not part of the microgrid.
    plain = _with_hybrid()
    del plain["commented"]
    assert _doc([_run(out.stdout)]) == _doc(specs_from_doc(plain))
    # Uncommented, they are in place under the meter.
    back = _run(_uncommented(out.stdout, "mc.make_battery_inverter("))
    [mg] = _doc([back])["microgrids"]
    assert [30, 31] == sorted(c["id"] for c in mg["components"] if c["id"] > 2)
    assert [2, 30] in mg["connections"] and [30, 31] in mg["connections"]


@pytest.mark.parametrize(
    ("key", "value", "reason"),
    [
        ("kind", "make_x(); import os #", "is not a kind of commented entry"),
        ("kind", ["hybrid_inverter"], "is not a kind of commented entry"),
        ("parent", 99, "parent 99 is not in the microgrid"),
        ("parent", True, "must be an id"),
        ("connections", [], "must be one tree"),
        ("connections", [[30, 31], [30, 31]], "each component under one parent"),
        ("connections", [[30, [31]]], "must be [parent, child]"),
        ("connections", [[30]], "must be [parent, child]"),
        ("connections", [[1, 31]], "names component 1, which is not in the entry"),
        (
            "components",
            [
                {"type": "battery_inverter", "id": 30},
                {"type": "battery", "id": 31},
                {"type": "battery", "id": 31},
            ],
            "the commented entry under 2: component id 31 appears twice",
        ),
        (
            "components",
            [
                {"type": "battery_inverter", "id": 30},
                {"type": "battery", "id": 31},
                {"type": "battery"},
            ],
            "the commented entry under 2: a battery has no id",
        ),
        (
            "components",
            [
                {"type": "battery_inverter", "id": 30, "name": 5},
                {"type": "battery", "id": 31},
            ],
            "the commented entry under 2 component 30 name",
        ),
        ("components", {"id": 30}, "must be lists"),
    ],
)
def test_a_bad_commented_entry_exits_1_with_the_reason(
    key: str, value: Any, reason: str
) -> None:
    doc = _with_hybrid()
    doc["commented"][0][key] = value
    out = _export(doc)
    assert out.returncode == 1
    assert out.stdout == ""
    assert reason in out.stderr


def test_a_commented_entry_needs_a_parent_that_takes_successors() -> None:
    doc = _with_hybrid()
    doc["microgrids"][0]["components"].append({"type": "chp", "id": 3})
    doc["microgrids"][0]["connections"].append([1, 3])
    doc["commented"][0]["parent"] = 3
    out = _export(doc)
    assert out.returncode == 1
    assert "parent 3 is a chp, which takes no successors" in out.stderr


@pytest.mark.parametrize("commented", [{}, [1], [{"parent": 2}]])
def test_bad_commented_lists_exit_1(commented: Any) -> None:
    doc = _with_hybrid()
    doc["commented"] = commented
    out = _export(doc)
    assert out.returncode == 1
    assert out.stderr.startswith("macrocosim.export: "), out.stderr


def test_a_commented_entry_may_not_reuse_an_id() -> None:
    doc = _with_hybrid()
    doc["commented"][0]["components"][0]["id"] = 2
    doc["commented"][0]["connections"] = [[30, 2]]
    out = _export(doc)
    assert out.returncode == 1
    assert "the commented entry under 2 reuses ids [2]" in out.stderr


def test_two_commented_entries_may_not_share_ids() -> None:
    doc = _with_hybrid()
    doc["commented"].append(doc["commented"][0])
    out = _export(doc)
    assert out.returncode == 1
    assert "the commented entry under 2 reuses ids [30, 31]" in out.stderr


def test_an_inverter_of_no_type_is_written_as_a_battery_inverter() -> None:
    doc = _with_hybrid()
    doc["commented"][0]["kind"] = "inverter_of_no_type"
    out = _export(doc)
    assert out.returncode == 0, out.stderr
    assert (
        "        # Needs an inverter type; written as a battery inverter:\n" in out.stdout
    )
    assert "        # mc.make_battery_inverter(\n" in out.stdout
