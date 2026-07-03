"""Demo of the ``switchyard_scenario`` marker: the named registered
scenario is run and gated after the test body."""

from __future__ import annotations

from pathlib import Path

import pytest

import switchyard as sw

# The scenario is defined in examples/scenario.lisp (topology + define-scenario).
SCENARIO_CONFIG = Path(__file__).parent.parent / "scenario.lisp"


@pytest.fixture
def switchyard_config() -> Path:
    return SCENARIO_CONFIG


@pytest.mark.switchyard_scenario("hold-load")
def test_hold_load_scenario_gate(switchyard: sw.Site) -> None:
    # The marker runs "hold-load" and asserts its checks after this returns;
    # the body can also drive/assert directly before the scenario runs.
    assert "hold-load" in [s["name"] for s in switchyard._http.get_json("/api/scenarios")]
