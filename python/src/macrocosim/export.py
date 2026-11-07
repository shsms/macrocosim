"""Write microgrid specs as a Python module: ``python -m macrocosim.export``.

Reads a topology document on stdin and writes, on stdout, a module that builds
the same microgrids with the ``make_*`` constructors and lists them in
``MICROGRIDS``, as a script for ``macrocosim plant.py`` does. The simulator runs
this for its "Save as .py" export.

Every component's id is written, so a script built from the export keeps the ids
the simulator showed. Every string is written with `repr`, so a name cannot
break out of its literal. A component under two parents is written once, as a
variable, before the microgrids.

Imports `_topology`, `_spec`, `_units` and `_errors`.
"""

from __future__ import annotations

import json
import sys
from datetime import timedelta
from enum import Enum
from typing import Any

from ._errors import SpecError
from ._spec import ComponentSpec, MicrogridSpec, _field_kinds, children
from ._topology import specs_from_doc
from ._units import UNITS, to_wire

INDENT = "    "


class _Writer:
    """The module's text, and the imports it needs."""

    def __init__(self, specs: list[MicrogridSpec]) -> None:
        self.quantities: set[str] = set()
        self.timedelta = False
        self.shared: dict[ComponentSpec[Any], str] = {}
        for mg in specs:
            parents: dict[ComponentSpec[Any], int] = {}
            for spec in mg._order:
                for child in children(spec):
                    parents[child] = parents.get(child, 0) + 1
            for spec in mg._order:
                if parents.get(spec, 0) > 1:
                    self.shared[spec] = f"{spec._type}_{mg._ids[spec]}"
        self.specs = specs

    def value(self, value: object) -> str:
        """A field value as a Python expression."""
        if isinstance(value, timedelta):
            self.timedelta = True
            return f"timedelta(seconds={to_wire(value)!r})"
        if isinstance(value, Enum):
            return f"mc.{type(value).__name__}.{value.name}"
        kind = type(value)
        unit = UNITS.get(kind)
        if unit is not None:
            constructor = getattr(unit.from_wire, "__qualname__", "")
            if kind.__module__.startswith("frequenz.quantities"):
                self.quantities.add(kind.__name__)
            else:
                constructor = f"mc.{constructor}"
            return f"{constructor}({unit.to_wire(value)!r})"
        if isinstance(value, (bool, int, float, str)):
            return repr(value)
        raise SpecError(f"cannot write {value!r} ({kind.__name__}) as Python")

    def component(self, mg: MicrogridSpec, spec: ComponentSpec[Any], depth: int) -> str:
        """`spec` as a ``make_*`` call, its successors nested."""
        args = [f"id={mg._ids[spec]!r}"]
        for name in _field_kinds(type(spec)):
            value = getattr(spec, name)
            if name in ("id", "successors") or value is None:
                continue
            args.append(f"{name}={self.value(value)}")
        kids = children(spec)
        head = f"mc.make_{spec._type}("
        if not kids:
            return head + ", ".join(args) + ")"
        pad = INDENT * (depth + 1)
        inner = INDENT * (depth + 2)
        lines = [head]
        lines += [f"{pad}{a}," for a in args]
        lines.append(f"{pad}successors=[")
        for child in kids:
            lines.append(f"{inner}{self.reference(mg, child, depth + 2)},")
        lines.append(f"{pad}],")
        lines.append(f"{INDENT * depth})")
        return "\n".join(lines)

    def reference(self, mg: MicrogridSpec, spec: ComponentSpec[Any], depth: int) -> str:
        """A shared component's variable, or the component written out."""
        return self.shared.get(spec) or self.component(mg, spec, depth)

    def microgrid(self, mg: MicrogridSpec) -> str:
        pad = INDENT * 2
        lines = [f"{INDENT}mc.make_microgrid("]
        if mg.id is not None:
            lines.append(f"{pad}id={int(mg.id)!r},")
        for name in ("name", "tso"):
            value = getattr(mg, name)
            if value is not None:
                lines.append(f"{pad}{name}={value!r},")
        if mg.grpc_port is not None:
            lines.append(f"{pad}grpc_port={int(mg.grpc_port)!r},")
        if mg.physics_tick is not None:
            lines.append(f"{pad}physics_tick={self.value(mg.physics_tick)},")
        topology = self.reference(mg, mg.topology, 2)
        lines.append(f"{pad}topology={topology},")
        lines.append(f"{INDENT}),")
        return "\n".join(lines)

    def module(self) -> str:
        variables = []
        for mg in self.specs:
            for spec in mg._order:
                name = self.shared.get(spec)
                if name is not None:
                    variables.append(f"{name} = {self.component(mg, spec, 0)}")
        body = ["MICROGRIDS = ["]
        body += [self.microgrid(mg) for mg in self.specs]
        body.append("]")
        # Written after the body, whose values name the imports.
        head = ['"""Microgrids exported from macrocosim."""', ""]
        if self.timedelta:
            head.append("from datetime import timedelta")
            head.append("")
        if self.quantities:
            head.append(
                f"from frequenz.quantities import {', '.join(sorted(self.quantities))}"
            )
            head.append("")
        head.append("import macrocosim as mc")
        parts = ["\n".join(head)]
        if variables:
            parts.append("\n\n".join(variables))
        parts.append("\n".join(body))
        return "\n\n".join(parts) + "\n"


def to_python(specs: list[MicrogridSpec]) -> str:
    """The text of a module whose ``MICROGRIDS`` builds `specs`."""
    return _Writer(specs).module()


def main() -> int:
    """Read a topology document on stdin and write its module on stdout."""
    try:
        # Bytes in and out: the simulator writes and reads UTF-8, whatever the
        # locale's encoding is.
        specs = specs_from_doc(json.load(sys.stdin.buffer))
        text = to_python(specs)
    except (SpecError, json.JSONDecodeError) as exc:
        print(f"macrocosim.export: {exc}", file=sys.stderr)
        return 1
    sys.stdout.buffer.write(text.encode("utf-8"))
    return 0


if __name__ == "__main__":
    sys.exit(main())
