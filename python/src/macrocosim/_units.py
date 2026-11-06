"""Value types the client accepts, and how each crosses the wire.

Internal; imports nothing from the package. One `_Unit` per supported value type
gives the suffix its Lisp keys carry and the conversions to and from the
base-unit float on the wire.
"""

from __future__ import annotations

import math
from collections.abc import Callable
from dataclasses import dataclass
from datetime import timedelta
from enum import Enum
from typing import Any

from frequenz.quantities import (
    ApparentPower,
    Current,
    Energy,
    Percentage,
    Power,
    ReactivePower,
    Voltage,
)


@dataclass(frozen=True)
class _Unit:
    """How one value type is named in Lisp, and carried as a float."""

    suffix: str
    """Appended to a Lisp key (``power`` → ``:power-w``)."""

    to_wire: Callable[[Any], float]
    from_wire: Callable[[float], Any]


UNITS: dict[type, _Unit] = {
    Power: _Unit("-w", lambda v: v.as_watts(), Power.from_watts),
    ReactivePower: _Unit(
        "-var",
        lambda v: v.as_volt_amperes_reactive(),
        ReactivePower.from_volt_amperes_reactive,
    ),
    ApparentPower: _Unit(
        "-va", lambda v: v.as_volt_amperes(), ApparentPower.from_volt_amperes
    ),
    Energy: _Unit("-wh", lambda v: v.as_watt_hours(), Energy.from_watt_hours),
    Percentage: _Unit("-pct", lambda v: v.as_percent(), Percentage.from_percent),
    Voltage: _Unit("-v", lambda v: v.as_volts(), Voltage.from_volts),
    Current: _Unit("-a", lambda v: v.as_amperes(), Current.from_amperes),
    timedelta: _Unit("-s", lambda v: v.total_seconds(), lambda s: timedelta(seconds=s)),
}
"""Every value type with a unit. Floats, ints, bools, strings and enums have
none and carry no suffix."""


def suffix_of(kind: type) -> str:
    """The Lisp key suffix for a field of type `kind` (``""`` for unitless)."""
    unit = UNITS.get(kind)
    return unit.suffix if unit else ""


def _type_name(kind: type | tuple[type, ...]) -> str:
    if isinstance(kind, tuple):
        return " or ".join(k.__name__ for k in kind)
    return kind.__name__


def check_type(field: str, value: object, expected: type | tuple[type, ...]) -> None:
    """Raise `TypeError` unless `value` is of the `expected` type.

    `bool` is refused where an `int` is expected (it is an `int` subclass, but
    never a sensible id or count); an `IntEnum` member is accepted there.
    """
    kinds = expected if isinstance(expected, tuple) else (expected,)
    if isinstance(value, bool) and bool not in kinds:
        ok = False
    elif float in kinds and is_int_like(value):
        ok = True
    else:
        ok = isinstance(value, kinds)
    if not ok:
        raise TypeError(
            f"{field} must be {_type_name(expected)}, not {type_and_value(value)}"
        )


def type_and_value(value: object) -> str:
    """``float (5.0)`` for a message: the type's name and, if it can be printed,
    the value."""
    text = repr_or_none(value)
    name = type(value).__name__
    return name if text is None else f"{name} ({text})"


def repr_or_none(value: object) -> str | None:
    """`repr(value)` for a message, or ``None`` when Python refuses to print it
    (an int of over 4300 digits, or a value that holds one)."""
    try:
        return repr(value)
    except ValueError:
        return None


def unit_of(value: object) -> _Unit | None:
    """The `UNITS` row of `value`'s type or of its nearest base class (a
    `timedelta` subclass has the `timedelta` row); ``None`` if unitless."""
    for cls in type(value).__mro__:
        unit = UNITS.get(cls)
        if unit is not None:
            return unit
    return None


def check_finite(field: str, value: object) -> None:
    """Raise `ValueError` if `value` is, or carries, NaN or infinity, or is an
    int too large for a float."""
    if isinstance(value, (str, Enum)):
        return
    if isinstance(value, (int, float)):
        try:
            number = float(value)
        except OverflowError:  # an int no float can hold
            raise ValueError(f"{field} is too large for a float") from None
    else:
        unit = unit_of(value)
        if unit is None:
            return
        number = unit.to_wire(value)
    if not math.isfinite(number):
        raise ValueError(f"{field} must be finite, not {value!r}")


def to_wire(value: object) -> float:
    """The base-unit float of a quantity or `timedelta`."""
    unit = unit_of(value)
    if unit is None:
        raise TypeError(f"{type(value).__name__} ({value!r}) has no unit")
    return unit.to_wire(value)


def is_int_like(value: object) -> bool:
    """Whether `value` is an `int` (an `IntEnum` member included) but not a
    `bool`."""
    return isinstance(value, int) and not isinstance(value, bool)
