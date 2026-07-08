"""pytest plugin: a ``switchyard`` fixture that launches and tears down a
Site for each test.

Auto-loaded via the ``pytest11`` entry point once the package is installed.
Provide a ``switchyard_config`` fixture (a :class:`switchyard.build.Microgrid`
or a path to a ``.lisp`` file) and depend on ``switchyard``:

    import switchyard as sw
    import pytest
    from frequenz.quantities import Power

    @pytest.fixture
    def switchyard_config():
        return sw.Microgrid(id=1, topology=sw.grid(id=1,
            successors=[sw.meter(id=2, power=7000.0)]))

    async def test_grid_holds(switchyard):
        await switchyard.expect.grid_power(
            approx=Power.from_kilowatts(7), tol=Power.from_watts(500))

The ``expect`` assertions are ``async``, so tests that await them must run
under ``pytest-asyncio`` (installed with the ``grpc`` extra) with
``asyncio_mode = "auto"`` in your pyproject/pytest config, or be marked
``@pytest.mark.asyncio`` individually. Without it pytest collects an
``async def`` test but never awaits it — the assertion silently never runs
and the test passes green.

The binary is found via ``SWITCHYARD_BIN`` / ``$PATH`` (or pass one through
a ``switchyard_bin`` fixture).
"""

from __future__ import annotations

from collections.abc import Iterator

import pytest

import switchyard as sw
from switchyard.build import LaunchConfig


def pytest_configure(config: pytest.Config) -> None:
    config.addinivalue_line(
        "markers",
        "switchyard_scenario(name): run the named registered scenario and "
        "fail the test on any failed check.",
    )


@pytest.fixture
def switchyard_bin() -> str | None:
    """Override to point at a specific binary; defaults to env/PATH lookup."""
    return None


@pytest.fixture
def switchyard_config() -> LaunchConfig:
    raise RuntimeError(
        "define a `switchyard_config` fixture returning a switchyard.Microgrid "
        "or a path to a .lisp config"
    )


@pytest.fixture
def switchyard(
    switchyard_config: LaunchConfig,
    switchyard_bin: str | None,
    request: pytest.FixtureRequest,
) -> Iterator[sw.Site]:
    """A freshly launched Site per test, torn down afterwards.

    If the test is marked ``@pytest.mark.switchyard_scenario("name")``, the
    named scenario is run and gated after the test body returns.
    """
    with sw.launch(switchyard_config, bin=switchyard_bin) as site:
        yield site
        marker = request.node.get_closest_marker("switchyard_scenario")
        if marker is not None:
            site.scenario(str(marker.args[0])).run(wait=True).assert_passed()
