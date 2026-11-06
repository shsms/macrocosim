"""The private modules import in one direction, and never the 0.1 modules."""

from __future__ import annotations

import ast
from pathlib import Path

import pytest

import macrocosim

pytestmark = pytest.mark.filterwarnings("error")

PACKAGE = Path(macrocosim.__file__).parent

ORDER = [
    "_units",
    "_enums",
    "_errors",
    "_matchers",
    "_spec",
    "_render",
    "_process",
    "_session",
    "_sampling",
    "_signals",
    "_components",
    "_microgrid",
    "_launch",
]
"""A module may import only modules listed before it."""

OLD = {
    "aio",
    "_grpc",
    "_http",
    "build",
    "runtime",
    "handles",
    "scenarios",
    "signals",
    "metrics",
    "assertions",
    "matchers",
    "errors",
    "enums",
    "pytest_plugin",
}
"""The 0.1 modules (with `aio/` standing for its four files) that the switch
deletes."""


def _is_type_checking(node: ast.If) -> bool:
    test = node.test
    return (isinstance(test, ast.Name) and test.id == "TYPE_CHECKING") or (
        isinstance(test, ast.Attribute) and test.attr == "TYPE_CHECKING"
    )


def _package_imports(path: Path) -> set[str]:
    """The package modules `path` imports at run time (not under TYPE_CHECKING)."""
    tree = ast.parse(path.read_text())
    found: set[str] = set()

    def walk(nodes: list[ast.stmt]) -> None:
        for node in nodes:
            if isinstance(node, ast.If) and _is_type_checking(node):
                walk(node.orelse)
                continue
            if isinstance(node, ast.ImportFrom):
                if node.level == 1:
                    if node.module:
                        found.add(node.module.split(".")[0])
                    else:
                        found.update(a.name for a in node.names)
                elif node.module and node.module.startswith("macrocosim."):
                    found.add(node.module.split(".")[1])
            elif isinstance(node, ast.Import):
                for alias in node.names:
                    if alias.name.startswith("macrocosim."):
                        found.add(alias.name.split(".")[1])
            for child in ("body", "orelse", "finalbody", "handlers"):
                inner = getattr(node, child, None)
                if isinstance(inner, list) and inner and isinstance(inner[0], ast.stmt):
                    walk(inner)

    walk(tree.body)
    return found


EXISTING = [m for m in ORDER if (PACKAGE / f"{m}.py").exists()]


@pytest.mark.parametrize("module", EXISTING)
def test_imports_only_modules_listed_before_it(module: str) -> None:
    allowed = set(ORDER[: ORDER.index(module)])
    imported = _package_imports(PACKAGE / f"{module}.py")
    assert not imported & OLD, f"{module} imports 0.1 modules {imported & OLD}"
    assert imported <= allowed, f"{module} imports {imported - allowed}"
