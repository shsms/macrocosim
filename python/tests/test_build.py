"""Emitter tests for the topology builder — pure, no binary required."""

from __future__ import annotations

from datetime import time, timedelta

from frequenz.quantities import Energy, Percentage, Power

import switchyard as sw
from switchyard.build import Microgrid, battery, battery_inverter, grid, meter, raw


def test_nested_successors_emit_make_forms() -> None:
    mg = Microgrid(
        id=1,
        topology=grid(id=1, successors=[meter(id=2, power=Power.from_watts(7000))]),
    )
    lisp = mg.to_lisp()
    assert "(make-microgrid :id 1" in lisp
    assert "(make-grid-connection-point :id 1 :successors (list" in lisp
    assert "(make-meter :id 2 :power 7000.0)" in lisp


def test_ergonomic_kwargs_map_to_plist_keys() -> None:
    # rated (a Power pair) → two keys in watts (inverters carry rated bounds).
    inv = battery_inverter(
        id=3, rated=(Power.from_kilowatts(-5), Power.from_kilowatts(5))
    )
    inv_lisp = inv.to_lisp()
    assert ":rated-lower -5000.0" in inv_lisp
    assert ":rated-upper 5000.0" in inv_lisp
    # Energy → Wh; Percentage → percent; soc → initial-soc (battery conveniences).
    bat_lisp = battery(
        id=4,
        capacity=Energy.from_kilowatt_hours(100),
        initial_soc=Percentage.from_percent(50),
        rated=(Power.from_kilowatts(-30), Power.from_kilowatts(30)),
    ).to_lisp()
    assert ":capacity 100000.0" in bat_lisp
    assert ":initial-soc 50.0" in bat_lisp
    assert ":rated-lower -30000.0" in bat_lisp
    assert ":rated-upper 30000.0" in bat_lisp


def test_value_kinds_render_correctly() -> None:
    # symbol-valued key (enum), string-valued key, bool → t, int stays int.
    node = meter(id=2, name="main", hidden=True, health=sw.Health.ERROR, interval=5)
    lisp = node.to_lisp()
    assert ':name "main"' in lisp
    assert ":hidden t" in lisp
    assert ":health 'error" in lisp  # symbol, not a string
    assert ":interval 5" in lisp  # int, no decimal


def test_to_lisp_atom_converts_typed_values() -> None:
    assert sw.to_lisp_atom(Power.from_kilowatts(2)) == "2000.0"
    assert sw.to_lisp_atom(Energy.from_kilowatt_hours(1)) == "1000.0"
    assert sw.to_lisp_atom(Percentage.from_percent(50)) == "50.0"
    assert sw.to_lisp_atom(timedelta(seconds=30)) == "30.0"
    assert sw.to_lisp_atom(time(12, 0)) == '"12:00:00"'
    assert sw.to_lisp_atom(sw.Health.ERROR) == "'error"
    assert sw.to_lisp_atom(True) == "t"
    assert sw.to_lisp_atom(5) == "5"


def test_raw_splices_literal_lisp() -> None:
    node = meter(id=2, power=raw("(lambda () (+ 1000.0 (random 500)))"))
    assert ":power (lambda () (+ 1000.0 (random 500)))" in node.to_lisp()


def test_branching_is_a_successors_list() -> None:
    mg = Microgrid(
        id=1,
        topology=grid(
            id=1,
            successors=[
                meter(id=2, successors=[battery_inverter(id=3)]),
                meter(id=5, power=Power.from_watts(-2000)),
            ],
        ),
    )
    lisp = mg.to_lisp()
    assert "(make-battery-inverter :id 3)" in lisp
    assert "(make-meter :id 5 :power -2000.0)" in lisp


def test_public_constructors_exported() -> None:
    for name in (
        "grid",
        "meter",
        "battery_inverter",
        "solar_inverter",
        "battery",
        "ev_charger",
        "chp",
        "raw",
        "Microgrid",
    ):
        assert hasattr(sw, name), name
