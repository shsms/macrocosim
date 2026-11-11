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
