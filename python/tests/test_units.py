"""Value types: wire conversions and the type and finiteness checks."""

from __future__ import annotations

from datetime import timedelta
from enum import IntEnum

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

from macrocosim._units import UNITS, check_finite, check_type, suffix_of

pytestmark = pytest.mark.filterwarnings("error")


@pytest.mark.parametrize(
    ("value", "wire", "suffix"),
    [
        (Power.from_kilowatts(2), 2000.0, "-w"),
        (ReactivePower.from_volt_amperes_reactive(150), 150.0, "-var"),
        (ApparentPower.from_volt_amperes(3000), 3000.0, "-va"),
        (Energy.from_kilowatt_hours(1), 1000.0, "-wh"),
        (Percentage.from_percent(55.5), 55.5, "-pct"),
        (Voltage.from_volts(800), 800.0, "-v"),
        (Current.from_amperes(16), 16.0, "-a"),
        (timedelta(milliseconds=200), 0.2, "-s"),
    ],
)
def test_every_row_converts_both_ways(value: object, wire: float, suffix: str) -> None:
    row = UNITS[type(value)]
    assert row.to_wire(value) == pytest.approx(wire)
    assert row.from_wire(wire) == value
    assert row.suffix == suffix == suffix_of(type(value))


@pytest.mark.parametrize("kind", [float, int, bool, str])
def test_unitless_types_have_no_suffix(kind: type) -> None:
    assert suffix_of(kind) == ""


def test_check_type_rejects_the_wrong_type() -> None:
    with pytest.raises(
        TypeError, match="^capacity must be Energy, not float \\(50000\\.0\\)$"
    ):
        check_type("capacity", 50000.0, Energy)
    with pytest.raises(TypeError, match="\\(18446744073709551616\\)$"):
        check_type("capacity", 2**64, Energy)
    with pytest.raises(TypeError, match="sunlight"):
        check_type("sunlight", Power.from_watts(1), Percentage)


def test_check_type_rejects_bool_for_int() -> None:
    with pytest.raises(TypeError, match="id must be int, not bool"):
        check_type("id", True, int)
    check_type("hidden", True, bool)


def test_check_type_accepts_an_intenum_for_int() -> None:
    class Ids(IntEnum):
        GRID = 1

    check_type("id", Ids.GRID, int)


def test_check_type_accepts_an_int_for_float() -> None:
    check_type("power_factor", 1, float)


@pytest.mark.parametrize(
    "value", [float("nan"), float("inf"), Power.from_watts(float("-inf"))]
)
def test_check_finite_rejects_nan_and_infinity(value: object) -> None:
    with pytest.raises(ValueError, match="x must be finite"):
        check_finite("x", value)


def test_check_finite_accepts_finite_values() -> None:
    for value in (0.5, 3, Power.from_watts(1), timedelta(seconds=1), "s", True):
        check_finite("x", value)
