"""The exact Lisp text a microgrid spec renders to."""

from __future__ import annotations

from datetime import timedelta
from enum import IntEnum

import pytest
from frequenz.quantities import (
    Current,
    Energy,
    Percentage,
    Power,
    ReactivePower,
)

import macrocosim._spec as s
from macrocosim._enums import Health
from macrocosim._render import render, to_lisp_atom

pytestmark = pytest.mark.filterwarnings("error")


def _mg(*successors: s.ComponentSpec, **kw: object) -> s.MicrogridSpec:
    gcp_id = kw.pop("gcp_id", None)
    return s.make_microgrid(
        id=7,
        topology=s.make_grid_connection_point(id=gcp_id, successors=list(successors)),
        **kw,  # type: ignore[arg-type]
    )


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
    assert render(mg) == (
        "(make-microgrid\n"
        " :id 7\n"
        ' :name "north"\n'
        ' :tso "tso-a"\n'
        " :topology\n"
        " (lambda ()\n"
        "   (let* ((c1000 (make-battery :id 1000 :capacity-wh 50000.0))\n"
        "          (c1001 (make-battery-inverter :id 1001 :successors (list c1000))))\n"
        "     (make-grid-connection-point :id 1002 :successors (list c1001)))))\n"
    )


def test_a_diamond_binds_the_shared_inverter_once() -> None:
    inv = s.make_battery_inverter(successors=[s.make_battery()])
    mg = _mg(s.make_meter(successors=[inv]), s.make_meter(successors=[inv]))
    assert render(mg) == (
        "(make-microgrid\n"
        " :id 7\n"
        " :topology\n"
        " (lambda ()\n"
        "   (let* ((c1000 (make-battery :id 1000))\n"
        "          (c1001 (make-battery-inverter :id 1001 :successors (list c1000)))\n"
        "          (c1002 (make-meter :id 1002 :successors (list c1001)))\n"
        "          (c1003 (make-meter :id 1003 :successors (list c1001))))\n"
        "     (make-grid-connection-point :id 1004 :successors (list c1002 c1003)))))\n"
    )


def test_a_shared_battery_under_two_inverters() -> None:
    bat = s.make_battery(id=30)
    mg = _mg(
        s.make_battery_inverter(id=10, successors=[bat]),
        s.make_battery_inverter(id=20, successors=[bat]),
        gcp_id=1,
    )
    assert render(mg).splitlines()[4:] == [
        "   (let* ((c30 (make-battery :id 30))",
        "          (c10 (make-battery-inverter :id 10 :successors (list c30)))",
        "          (c20 (make-battery-inverter :id 20 :successors (list c30))))",
        "     (make-grid-connection-point :id 1 :successors (list c10 c20)))))",
    ]


def test_partial_ids() -> None:
    mg = _mg(s.make_meter(), s.make_meter(id=1000), s.make_meter())
    assert render(mg).splitlines()[4:] == [
        "   (let* ((c1001 (make-meter :id 1001))",
        "          (c1000 (make-meter :id 1000))",
        "          (c1002 (make-meter :id 1002)))",
        "     (make-grid-connection-point :id 1003"
        " :successors (list c1001 c1000 c1002)))))",
    ]


def test_no_successors_gives_an_empty_let() -> None:
    mg = s.make_microgrid(topology=s.make_grid_connection_point(id=1, successors=[]))
    assert render(mg) == (
        "(make-microgrid\n"
        " :topology\n"
        " (lambda ()\n"
        "   (let* ()\n"
        "     (make-grid-connection-point :id 1))))\n"
    )


def test_integer_keys_render_without_a_decimal_point() -> None:
    mg = s.make_microgrid(
        id=7,
        topology=s.make_grid_connection_point(
            id=1, rated_fuse_current=Current.from_amperes(63)
        ),
    )
    assert "(make-grid-connection-point :id 1 :rated-fuse-current-a 63)" in render(mg)


def test_an_intenum_id_renders_as_its_number() -> None:
    class Ids(IntEnum):
        MG = 3
        GRID = 1

    mg = s.make_microgrid(id=Ids.MG, topology=s.make_grid_connection_point(id=Ids.GRID))
    text = render(mg)
    assert " :id 3\n" in text
    assert "(make-grid-connection-point :id 1)" in text


def test_nested_successors_emit_make_forms() -> None:
    mg = s.make_microgrid(
        id=7,
        topology=s.make_grid_connection_point(
            id=1, successors=[s.make_meter(id=2, power=Power.from_watts(7000))]
        ),
    )
    assert render(mg) == (
        "(make-microgrid\n"
        " :id 7\n"
        " :topology\n"
        " (lambda ()\n"
        "   (let* ((c2 (make-meter :id 2 :power-w 7000.0)))\n"
        "     (make-grid-connection-point :id 1 :successors (list c2)))))\n"
    )


def test_value_kinds_render_correctly() -> None:
    m = s.make_meter(
        id=2, name="main", hidden=True, health=Health.ERROR, interval=timedelta(seconds=5)
    )
    assert (
        '(c2 (make-meter :id 2 :name "main" :health \'error :interval-s 5.0 :hidden t))'
        in render(_mg(m))
    )


def test_meter_reactive_kwargs_render_correctly() -> None:
    text = render(
        _mg(
            s.make_meter(
                id=2, reactive_power=ReactivePower.from_volt_amperes_reactive(1500)
            ),
            s.make_meter(id=3, power_factor=0.8, leading=True),
            s.make_meter(id=4, power_factor=0.9),
        )
    )
    assert "(make-meter :id 2 :reactive-power-var 1500.0)" in text
    assert "(make-meter :id 3 :power-factor 0.8 :leading t)" in text
    assert "(make-meter :id 4 :power-factor 0.9)" in text


def test_branching_is_a_successors_list() -> None:
    lines = render(
        _mg(
            s.make_meter(id=2, successors=[s.make_battery_inverter(id=3)]),
            s.make_meter(id=5, power=Power.from_watts(-2000)),
            gcp_id=1,
        )
    ).splitlines()
    assert "   (let* ((c3 (make-battery-inverter :id 3))" in lines
    assert "          (c5 (make-meter :id 5 :power-w -2000.0)))" in lines


def test_typed_quantities_render_in_base_units() -> None:
    text = render(
        _mg(
            s.make_battery_inverter(
                id=2,
                command_delay=timedelta(milliseconds=200),
                successors=[
                    s.make_battery(id=1, capacity=Energy.from_kilowatt_hours(50))
                ],
            ),
            s.make_solar_inverter(id=3, sunlight=Percentage.from_percent(80)),
        )
    )
    assert ":capacity-wh 50000.0" in text
    assert ":command-delay-s 0.2" in text
    assert ":sunlight-pct 80.0" in text


def test_strings_are_escaped() -> None:
    text = render(
        s.make_microgrid(name='a "b" \\ c', topology=s.make_grid_connection_point())
    )
    assert ' :name "a \\"b\\" \\\\ c"\n' in text


def test_to_lisp_atom_renders_wire_values() -> None:
    class Ids(IntEnum):
        X = 7

    assert to_lisp_atom(2000.0) == "2000.0"
    assert to_lisp_atom(Health.ERROR) == "'error"
    assert to_lisp_atom(True) == "t"
    assert to_lisp_atom(False) == "nil"
    assert to_lisp_atom(5) == "5"
    assert to_lisp_atom(Ids.X) == "7"
    assert to_lisp_atom("x") == '"x"'


def test_to_lisp_atom_refuses_a_quantity() -> None:
    with pytest.raises(TypeError, match="cannot render Power"):
        to_lisp_atom(Power.from_kilowatts(2))
