"""Spec construction: types, checks, identity and `plist`."""

from __future__ import annotations

import dataclasses
import typing
from datetime import timedelta
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
from macrocosim._enums import Health
from macrocosim._errors import SpecError
from macrocosim._units import UNITS

pytestmark = pytest.mark.filterwarnings("error")

kW = Power.from_kilowatts

COMPONENT_SPECS = [
    s.GridConnectionPointSpec,
    s.MeterSpec,
    s.BatterySpec,
    s.BatteryInverterSpec,
    s.SolarInverterSpec,
    s.ChpSpec,
]


def test_constructors_are_the_classes() -> None:
    assert s.make_meter is s.MeterSpec
    assert s.make_microgrid is s.MicrogridSpec
    assert s.make_grid_connection_point is s.GridConnectionPointSpec


def test_specs_hash_and_compare_by_identity() -> None:
    a, b = s.make_meter(power=kW(1)), s.make_meter(power=kW(1))
    assert a != b
    assert a == a
    assert len({a, b}) == 2


def test_specs_are_frozen() -> None:
    m = s.make_meter()
    with pytest.raises(dataclasses.FrozenInstanceError):
        m.power = kW(1)  # type: ignore[misc]


def test_successors_become_a_tuple() -> None:
    child = s.make_meter()
    parent = s.make_meter(successors=[child])
    assert parent.successors == (child,)


def test_constructors_take_keywords_only() -> None:
    with pytest.raises(TypeError):
        s.make_meter(1)  # type: ignore[misc]


def test_ergonomic_kwargs_map_to_plist_keys() -> None:
    inv = s.make_battery_inverter(id=3, rated_lower=kW(-5), rated_upper=kW(5))
    assert s.plist(inv) == [(":rated-lower-w", -5000.0), (":rated-upper-w", 5000.0)]
    bat = s.make_battery(
        id=4,
        capacity=Energy.from_kilowatt_hours(100),
        initial_soc=Percentage.from_percent(50),
        rated_lower=kW(-30),
        rated_upper=kW(30),
    )
    assert s.plist(bat) == [
        (":capacity-wh", 100000.0),
        (":initial-soc-pct", 50.0),
        (":rated-lower-w", -30000.0),
        (":rated-upper-w", 30000.0),
    ]


def test_plist_keeps_field_order_base_fields_first() -> None:
    m = s.make_meter(power=kW(2), name="load", health=Health.OK, hidden=True)
    assert s.plist(m) == [
        (":name", "load"),
        (":health", Health.OK),
        (":power-w", 2000.0),
        (":hidden", True),
    ]


def test_plist_sends_current_as_whole_amperes() -> None:
    gcp = s.make_grid_connection_point(rated_fuse_current=Current.from_amperes(63))
    [(key, value)] = s.plist(gcp)
    assert key == ":rated-fuse-current-a"
    assert value == 63 and type(value) is int


def test_a_bare_number_where_a_quantity_belongs_raises() -> None:
    with pytest.raises(TypeError, match="capacity"):
        s.make_battery(capacity=50000.0)  # type: ignore[arg-type]
    with pytest.raises(TypeError, match="initial_soc"):
        s.make_battery(initial_soc=60.0)  # type: ignore[arg-type]
    with pytest.raises(TypeError, match="interval"):
        s.make_meter(interval=1.0)  # type: ignore[arg-type]
    with pytest.raises(TypeError, match="stream_jitter"):
        s.make_grid_connection_point(stream_jitter=5.0)  # type: ignore[arg-type]
    with pytest.raises(TypeError, match="rated_lower"):
        s.make_battery_inverter(rated_lower=1.0)  # type: ignore[arg-type]


def test_sunlight_takes_a_percentage() -> None:
    sun = s.make_solar_inverter(id=1, sunlight=Percentage.from_percent(80))
    assert s.plist(sun) == [(":sunlight-pct", 80.0)]
    with pytest.raises(TypeError, match="sunlight"):
        s.make_solar_inverter(id=1, sunlight=80.0)  # type: ignore[arg-type]


def test_a_bool_is_not_an_id() -> None:
    with pytest.raises(TypeError, match="id must be int, not bool"):
        s.make_meter(id=True)


def test_an_enum_field_needs_the_member() -> None:
    with pytest.raises(TypeError, match="health must be Health"):
        s.make_meter(health="error")  # type: ignore[arg-type]


def test_a_non_finite_number_raises_value_error() -> None:
    with pytest.raises(ValueError, match="power must be finite"):
        s.make_meter(power=Power.from_watts(float("nan")))
    with pytest.raises(ValueError, match="power_factor must be finite"):
        s.make_meter(power_factor=float("inf"))


@pytest.mark.parametrize("bad", [0, -3])
def test_an_id_of_zero_or_less_raises(bad: int) -> None:
    with pytest.raises(
        SpecError, match=f"id must be between 1 and 2\\*\\*63 - 1, not {bad}$"
    ):
        s.make_meter(id=bad)


@pytest.mark.parametrize("bad", [0, -1])
def test_a_microgrid_id_of_zero_or_less_raises(bad: int) -> None:
    with pytest.raises(SpecError, match="id must be between 1 and 2\\*\\*63 - 1"):
        s.make_microgrid(id=bad, topology=s.make_grid_connection_point())


def test_a_fractional_fuse_current_raises() -> None:
    with pytest.raises(SpecError, match="whole amperes"):
        s.make_grid_connection_point(rated_fuse_current=Current.from_amperes(16.5))


def test_reactive_power_and_power_factor_exclude_each_other() -> None:
    with pytest.raises(SpecError, match="cannot both be set"):
        s.make_meter(
            reactive_power=ReactivePower.from_volt_amperes_reactive(1),
            power_factor=0.9,
        )


def test_leading_needs_power_factor() -> None:
    with pytest.raises(SpecError, match="leading needs power_factor"):
        s.make_meter(leading=True)


def test_the_same_spec_twice_under_one_parent_raises() -> None:
    m = s.make_meter()
    with pytest.raises(SpecError, match="appears twice"):
        s.make_grid_connection_point(successors=[m, m])


def test_successors_must_be_component_specs() -> None:
    with pytest.raises(TypeError, match="successors must hold component specs"):
        s.make_meter(successors=["x"])  # type: ignore[list-item]


def test_a_microgrid_needs_a_topology() -> None:
    with pytest.raises(TypeError):
        s.make_microgrid(id=1)  # type: ignore[call-arg]


def test_the_topology_must_be_a_grid_connection_point() -> None:
    with pytest.raises(TypeError, match="topology must be a GridConnectionPointSpec"):
        s.make_microgrid(topology=s.make_meter())  # type: ignore[arg-type]


def test_repr_shows_id_and_set_fields() -> None:
    assert repr(s.make_meter(power=kW(10))) == "MeterSpec(id=auto, power=10 kW)"
    assert (
        repr(s.make_meter(id=4, health=Health.ERROR, interval=timedelta(seconds=1)))
        == "MeterSpec(id=4, health=Health.ERROR, interval=datetime.timedelta(seconds=1))"
    )


def _fields() -> list[tuple[type, str, Any]]:
    out = []
    for cls in COMPONENT_SPECS:
        hints = typing.get_type_hints(cls)
        for f in dataclasses.fields(cls):
            if f.init:
                out.append((cls, f.name, hints[f.name]))
    return out


@pytest.mark.parametrize(
    ("cls", "name", "hint"), _fields(), ids=lambda v: getattr(v, "__name__", str(v))
)
def test_lisp_key_suffix_matches_type(cls: type, name: str, hint: Any) -> None:
    key = s.lisp_key(cls, name)
    kinds = [a for a in typing.get_args(hint) if a is not type(None)]
    kind = kinds[0] if kinds else hint
    row = UNITS.get(kind)
    base = ":" + name.replace("_", "-")
    if row is None:
        assert key == base
    else:
        assert key == base + row.suffix


def test_ids_must_fit_in_64_bits() -> None:
    s.make_meter(id=2**63 - 1)
    with pytest.raises(SpecError, match=", not 9223372036854775808$"):
        s.make_meter(id=2**63)
    with pytest.raises(SpecError, match=", not 18446744073709551616$"):
        s.make_meter(id=2**64)
    with pytest.raises(SpecError, match="between 1 and 2\\*\\*63 - 1$"):
        s.make_meter(id=10**5000)  # too long for repr
    with pytest.raises(SpecError, match="between 1 and"):
        s.make_microgrid(id=2**63, topology=s.make_grid_connection_point())
    with pytest.raises(SpecError, match="between 1 and"):
        s.make_microgrid(id=10**5000, topology=s.make_grid_connection_point())


def test_an_automatic_id_past_the_largest_raises() -> None:
    top = s.make_grid_connection_point(
        successors=[s.make_meter(id=2**63 - 1), s.make_meter()]
    )
    with pytest.raises(SpecError, match="the next automatic id would pass 2"):
        s.make_microgrid(topology=top)


def test_the_largest_id_can_be_automatic() -> None:
    last = s.make_meter()
    top = s.make_grid_connection_point(
        id=1, successors=[s.make_meter(id=2**63 - 2), last]
    )
    assert s.make_microgrid(topology=top)._ids[last] == 2**63 - 1


def test_an_int_no_float_can_hold_raises_where_the_spec_is_built() -> None:
    with pytest.raises(ValueError, match="power_factor is too large for a float"):
        s.make_meter(power_factor=10**400)


def test_a_half_rated_band_on_the_grid_connection_point_raises() -> None:
    with pytest.raises(SpecError, match="both rated_lower and rated_upper"):
        s.make_grid_connection_point(rated_lower=Power.from_kilowatts(-10))
    s.make_grid_connection_point(
        rated_lower=Power.from_kilowatts(-10), rated_upper=Power.from_kilowatts(10)
    )


def test_the_base_class_cannot_be_built() -> None:
    with pytest.raises(TypeError, match="make_\\* constructor"):
        s.ComponentSpec(id=5)


def test_an_int_for_a_float_field_goes_out_as_a_float() -> None:
    [(key, value)] = s.plist(s.make_meter(power_factor=1))
    assert key == ":power-factor"
    assert type(value) is float and value == 1.0


def test_a_timedelta_subclass_has_the_timedelta_unit() -> None:
    class Seconds(timedelta):
        pass

    [(key, value)] = s.plist(s.make_meter(interval=Seconds(seconds=2)))
    assert (key, value) == (":interval-s", 2.0)


def test_a_huge_int_in_a_quantity_field_is_a_type_error() -> None:
    with pytest.raises(TypeError, match="^power must be Power, not int$"):
        s.make_meter(power=10**5000)
    with pytest.raises(TypeError, match="^power must be Power, not list$"):
        s.make_meter(power=[10**5000])  # type: ignore[arg-type]
