"""Render a `MicrogridSpec` as the Lisp text the simulator boots from.

Internal; imports `_spec` and `_units`. The topology is one flat ``let*`` with a
binding per spec object, children before parents and every id written out, so a
spec shared by two parents is built once and referred to twice. It calls the
``make-*`` wrappers, so the simulator's defaults still apply.
"""

from __future__ import annotations

from enum import StrEnum
from typing import Any, cast

from ._spec import ComponentSpec, MicrogridSpec, children, plist
from ._units import is_int_like, type_and_value


def _lisp_string(value: str) -> str:
    escaped = value.replace("\\", "\\\\").replace('"', '\\"')
    return f'"{escaped}"'


def to_lisp_atom(value: object) -> str:
    """Render one wire value (see `plist`) as the Lisp literal the simulator reads.

    `bool` → ``t``/``nil``; a `StrEnum` → a quoted symbol; an `int` (an
    `IntEnum` included) → its digits; a `float` → `repr`; a `str` → a quoted,
    escaped string.
    """
    if isinstance(value, bool):
        return "t" if value else "nil"
    if isinstance(value, StrEnum):
        return f"'{value}"
    if is_int_like(value):
        return str(int(cast("int", value)))
    if isinstance(value, float):
        return repr(value)
    if isinstance(value, str):
        return _lisp_string(value)
    raise TypeError(f"cannot render {type_and_value(value)} as Lisp")


def _form(spec: ComponentSpec[Any], ids: dict[ComponentSpec[Any], int]) -> str:
    parts = [f"({spec._make}", f":id {ids[spec]}"]
    for key, value in plist(spec):
        parts.append(f"{key} {to_lisp_atom(value)}")
    successors = children(spec)
    if successors:
        refs = " ".join(f"c{ids[child]}" for child in successors)
        parts.append(f":successors (list {refs})")
    return " ".join(parts) + ")"


def render(spec: MicrogridSpec) -> str:
    """The ``(make-microgrid ...)`` form for `spec`, ending in a newline."""
    ids = spec._ids
    lines = ["(make-microgrid"]
    if spec.id is not None:
        lines.append(f" :id {int(spec.id)}")
    if spec.name is not None:
        lines.append(f" :name {_lisp_string(spec.name)}")
    if spec.tso is not None:
        lines.append(f" :tso {_lisp_string(spec.tso)}")
    lines.append(" :topology")
    lines.append(" (lambda ()")

    root = spec.topology
    bindings = [f"(c{ids[c]} {_form(c, ids)})" for c in spec._order if c is not root]
    if not bindings:
        lines.append("   (let* ()")
    else:
        lines.append(f"   (let* ({bindings[0]}")
        lines.extend(f"          {b}" for b in bindings[1:])
        lines[-1] += ")"
    lines.append(f"     {_form(root, ids)})))")
    return "\n".join(lines) + "\n"
