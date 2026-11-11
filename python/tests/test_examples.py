"""The scripts in the repository's examples/ build their microgrids, and can all
run against one simulator together, beside the UI smoke's site. The tests in
examples/testing/ launch their own simulators; CI runs them with pytest."""

from __future__ import annotations

import functools
import importlib.util
from pathlib import Path
from types import ModuleType

import pytest

import macrocosim as mc
from macrocosim._render import render
from macrocosim._spec import _check_explicit_ids

pytestmark = pytest.mark.filterwarnings("error")

REPO = Path(__file__).resolve().parents[2]
EXAMPLES = REPO / "examples"
SCRIPTS = sorted(
    p for p in EXAMPLES.rglob("*.py") if p.relative_to(EXAMPLES).parts[0] != "testing"
)
SMOKE_SITE = REPO / "tools" / "ui-smoke" / "site.py"
"""The UI smoke's site, which runs beside the examples."""


@functools.cache
def _microgrids(path: Path) -> tuple[mc.MicrogridSpec, ...]:
    """The microgrid specs the script at `path` defines; importing it runs none
    of its `main`."""
    name = "example_" + "_".join(path.relative_to(REPO).with_suffix("").parts)
    spec = importlib.util.spec_from_file_location(name.replace("-", "_"), path)
    assert spec is not None and spec.loader is not None
    module: ModuleType = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return tuple(v for v in vars(module).values() if isinstance(v, mc.MicrogridSpec))


def test_there_are_examples() -> None:
    assert len(SCRIPTS) >= 10, SCRIPTS


@pytest.mark.parametrize("path", SCRIPTS, ids=lambda p: str(p.relative_to(EXAMPLES)))
def test_an_example_builds_its_microgrids(path: Path) -> None:
    microgrids = _microgrids(path)
    assert microgrids, f"{path.name} defines no MicrogridSpec"
    render(*microgrids)
    for microgrid in microgrids:
        assert microgrid.id is not None, f"{path.name}: {microgrid!r} has no id"
        missing = [c for c in microgrid._order if c.id is None]
        assert not missing, f"{path.name}: give these components ids: {missing}"


def test_the_examples_share_no_ids() -> None:
    microgrids = [mg for path in (SMOKE_SITE, *SCRIPTS) for mg in _microgrids(path)]
    ids = [int(mg.id or 0) for mg in microgrids]
    assert len(set(ids)) == len(ids), f"a microgrid id is used twice: {sorted(ids)}"
    # The rule `declare` applies: no component id in two microgrids.
    _check_explicit_ids(microgrids)
