"""Write microgrid specs as a Python module: ``python -m macrocosim.export``.

Reads a topology document of one microgrid on stdin and writes, on stdout, a
module that builds the same microgrid with the ``make_*`` constructors as
``MICROGRID``, and declares it on a running simulator when run as a program
(``python site.py``). Importing the module declares nothing. The simulator runs
this for its "Save as .py" export.

Every component's id is written, so a script built from the export keeps the ids
the simulator showed. Every string is written with `repr`, so a name cannot
break out of its literal. A component under two parents is written once, as a
variable, before the microgrid.

The document may also carry ``commented``: a list of entries, each a tree of
components the simulator cannot model as they are. Each entry is written as
commented-out lines in its parent's ``successors``, so that uncommenting it puts
it in place. The module imports what those lines need.

Each entry is an object with ``parent`` (the id of a component of the microgrid
that takes successors), ``kind`` (``hybrid_inverter`` or
``inverter_of_no_type``), and ``components`` and ``connections`` as in a
microgrid. They form one tree, each component under one parent. Its root is
written with the kind's constructor, such as ``make_hybrid_inverter``, and with
the fields of its own ``type``. Its ids are not in the microgrid or in another
entry.

Imports `_topology`, `_spec`, `_units` and `_errors`.
"""

from __future__ import annotations

import itertools
import json
import sys
from datetime import timedelta
from enum import Enum
from typing import Any

from ._errors import SpecError
from ._spec import ComponentSpec, MicrogridSpec, _field_kinds, children
from ._topology import microgrid_from_value, specs_from_doc
from ._units import UNITS, is_int_like, to_wire

INDENT = "    "


class _Writer:
    """The module's text, and the imports it needs."""

    def __init__(self, mg: MicrogridSpec) -> None:
        self.quantities: set[str] = set()
        self.timedelta = False
        self.shared: dict[ComponentSpec[Any], str] = {}
        # The commented-out lines written in a component's successors, by id.
        self.commented_lines: dict[int, list[list[str]]] = {}
        parents: dict[ComponentSpec[Any], int] = {}
        for spec in mg._order:
            for child in children(spec):
                parents[child] = parents.get(child, 0) + 1
        for spec in mg._order:
            if parents.get(spec, 0) > 1:
                self.shared[spec] = f"{spec._type}_{mg._ids[spec]}"
        self.mg = mg

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

    def component(
        self,
        mg: MicrogridSpec,
        spec: ComponentSpec[Any],
        depth: int,
        constructor: str | None = None,
    ) -> str:
        """`spec` as a ``make_*`` call (or a `constructor` call), its successors
        nested."""
        args = [f"id={mg._ids[spec]!r}"]
        for name in _field_kinds(type(spec)):
            value = getattr(spec, name)
            if name in ("id", "successors") or value is None:
                continue
            args.append(f"{name}={self.value(value)}")
        kids = children(spec)
        blocks = self.commented_lines.get(mg._ids[spec], [])
        head = f"mc.{constructor or 'make_' + spec._type}("
        if not kids and not blocks:
            return head + ", ".join(args) + ")"
        pad = INDENT * (depth + 1)
        inner = INDENT * (depth + 2)
        lines = [head]
        lines += [f"{pad}{a}," for a in args]
        lines.append(f"{pad}successors=[")
        for child in kids:
            lines.append(f"{inner}{self.reference(mg, child, depth + 2)},")
        for block in blocks:
            lines += [f"{inner}# {line}" for line in block]
        lines.append(f"{pad}],")
        lines.append(f"{INDENT * depth})")
        return "\n".join(lines)

    def reference(self, mg: MicrogridSpec, spec: ComponentSpec[Any], depth: int) -> str:
        """A shared component's variable, or the component written out."""
        return self.shared.get(spec) or self.component(mg, spec, depth)

    def microgrid(self, mg: MicrogridSpec) -> str:
        lines = ["MICROGRID = mc.make_microgrid("]
        if mg.id is not None:
            lines.append(f"{INDENT}id={int(mg.id)!r},")
        for name in ("name", "tso"):
            value = getattr(mg, name)
            if value is not None:
                lines.append(f"{INDENT}{name}={value!r},")
        if mg.grpc_port is not None:
            lines.append(f"{INDENT}grpc_port={int(mg.grpc_port)!r},")
        if mg.physics_tick is not None:
            lines.append(f"{INDENT}physics_tick={self.value(mg.physics_tick)},")
        topology = self.reference(mg, mg.topology, 1)
        lines.append(f"{INDENT}topology={topology},")
        lines.append(")")
        return "\n".join(lines)

    def module(self) -> str:
        mg = self.mg
        variables = []
        for spec in mg._order:
            name = self.shared.get(spec)
            if name is not None:
                variables.append(f"{name} = {self.component(mg, spec, 0)}")
        body = self.microgrid(mg)
        # Written after the body, whose values name the imports.
        head = [
            '"""A microgrid exported from macrocosim: import ``MICROGRID``, or run',
            "this file to declare it on the simulator at ``$MACROCOSIM_URL``",
            '(``http://127.0.0.1:8801`` when unset)."""',
            "",
            "import asyncio",
        ]
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
        parts.append(body)
        return "\n\n".join(parts) + "\n" + MAIN_BLOCK


MAIN_BLOCK = """

async def main() -> None:
    async with mc.connect() as sim:
        await sim.declare(MICROGRID)


if __name__ == "__main__":
    asyncio.run(main())
"""
"""The end of every exported module: declare ``MICROGRID`` when run."""


# Each kind of commented entry's constructor, and the note written above it.
_KINDS = {
    "hybrid_inverter": ("make_hybrid_inverter", "Needs a hybrid inverter model:"),
    "inverter_of_no_type": (
        "make_battery_inverter",
        "Needs an inverter type; written as a battery inverter:",
    ),
}


def _commented_lines(
    writer: _Writer, entry: Any, used: set[int]
) -> tuple[int, list[str]]:
    """One ``commented`` entry as its parent's id and its lines, not yet
    commented out. The imports its values need go to `writer`. Its ids are added
    to `used`; one already there raises `SpecError`."""
    keys = {"parent", "kind", "components", "connections"}
    if not isinstance(entry, dict) or set(entry) != keys:
        raise SpecError(f"a commented entry must be an object with keys {sorted(keys)}")
    parent, kind = entry["parent"], entry["kind"]
    if not isinstance(parent, int) or isinstance(parent, bool):
        raise SpecError(f"a commented entry's parent must be an id, not {parent!r}")
    if not isinstance(kind, str) or kind not in _KINDS:
        raise SpecError(f"{kind!r} is not a kind of commented entry: {sorted(_KINDS)}")
    constructor, note = _KINDS[kind]
    components, connections = entry["components"], entry["connections"]
    if not isinstance(components, list) or not isinstance(connections, list):
        raise SpecError("a commented entry's components and connections must be lists")
    # A component with no id is left to the stand-in microgrid to refuse.
    ids = [c["id"] for c in components if isinstance(c, dict) and "id" in c]
    for pair in connections:
        if not (
            isinstance(pair, list)
            and len(pair) == 2
            and all(is_int_like(x) for x in pair)
        ):
            raise SpecError(
                f"a commented entry's connection must be [parent, child], not {pair!r}"
            )
        for end in pair:
            if end not in ids:
                raise SpecError(
                    f"a commented entry's connection {pair!r} names component {end}, "
                    "which is not in the entry"
                )
    under = [child for _, child in connections]
    # A repeated id counts once here; the stand-in microgrid refuses it.
    tops = [i for k, i in enumerate(ids) if i not in under and i not in ids[:k]]
    if len(tops) != 1 or len(under) != len(set(under)):
        raise SpecError(
            "a commented entry must be one tree, each component under one parent"
        )
    # The tree under a stand-in grid connection point, so it reads as a
    # microgrid.
    grid = next(i for i in itertools.count(1) if i not in ids)
    mg = microgrid_from_value(
        {
            "components": [
                {"type": "grid_connection_point", "id": grid},
                *components,
            ],
            "connections": [*connections, [grid, tops[0]]],
        },
        label=f"the commented entry under {parent}",
    )
    tree_ids = set(mg._ids.values()) - {grid}
    if tree_ids & used:
        raise SpecError(
            f"the commented entry under {parent} reuses ids {sorted(tree_ids & used)}"
        )
    used |= tree_ids
    [top] = children(mg.topology)
    text = writer.component(mg, top, 0, constructor)
    return parent, [note, *f"{text},".split("\n")]


def to_python(specs: list[MicrogridSpec], commented: Any = None) -> str:
    """The text of a module whose ``MICROGRID`` builds the one microgrid of
    `specs`, and whose ``main`` declares it. Each `commented` entry (see the
    module docs) is written as commented-out lines under its parent. More or
    fewer than one microgrid raises `SpecError`: the export writes one microgrid
    per file."""
    if len(specs) != 1:
        raise SpecError(f"the export writes one microgrid per file, not {len(specs)}")
    writer = _Writer(specs[0])
    if commented is not None:
        if not isinstance(commented, list):
            raise SpecError("commented must be a list")
        by_id = {cid: spec for spec, cid in specs[0]._ids.items()}
        used = set(by_id)
        for entry in commented:
            parent, lines = _commented_lines(writer, entry, used)
            spec = by_id.get(parent)
            if spec is None:
                raise SpecError(
                    f"a commented entry's parent {parent} is not in the microgrid"
                )
            if "successors" not in _field_kinds(type(spec)):
                raise SpecError(
                    f"a commented entry's parent {parent} is a {spec._type}, which "
                    "takes no successors"
                )
            writer.commented_lines.setdefault(parent, []).append(lines)
    return writer.module()


def main() -> int:
    """Read a topology document on stdin and write its module on stdout."""
    try:
        # Bytes in and out: the simulator writes and reads UTF-8, whatever the
        # locale's encoding is.
        doc = json.load(sys.stdin.buffer)
        commented = doc.pop("commented", None) if isinstance(doc, dict) else None
        text = to_python(specs_from_doc(doc), commented)
    except (SpecError, json.JSONDecodeError) as exc:
        print(f"macrocosim.export: {exc}", file=sys.stderr)
        return 1
    sys.stdout.buffer.write(text.encode("utf-8"))
    return 0


if __name__ == "__main__":
    sys.exit(main())
