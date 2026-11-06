"""Specs against a real simulator: key coverage, and what `render` boots.

The boot half launches through 0.1's ``macrocosim.aio.launch``, which takes
any object with a ``to_lisp()`` method; the switch to the new client deletes
that half (``test_live.py`` covers the same trees through ``mc.launch``).
"""

from __future__ import annotations

from collections.abc import AsyncIterator
from typing import Any

import httpx
import pytest
import pytest_asyncio
from topology_check import topology_problems

import macrocosim as mc
import macrocosim._spec as s
from macrocosim._render import render

pytestmark = [
    pytest.mark.live,
    pytest.mark.filterwarnings("ignore:.*ComponentId is deprecated.*:DeprecationWarning"),
]

PRIMITIVE = {
    s.GridConnectionPointSpec: "%make-grid-connection-point",
    s.MeterSpec: "%make-meter",
    s.BatterySpec: "%make-battery",
    s.BatteryInverterSpec: "%make-battery-inverter",
    s.SolarInverterSpec: "%make-solar-inverter",
    s.ChpSpec: "%make-chp",
    s.MicrogridSpec: "make-microgrid",
}
"""The server symbol whose keys each spec class must cover. The microgrid's
constructor is a Rust defun with no ``%`` form."""

NOT_EXPOSED = {s.MicrogridSpec: {":grpc-port"}}
"""Server keys deliberately absent from the specs (launch binds ephemeral ports)."""


class _Rendered:
    """Hands `render`'s text to 0.1's launch, which calls ``to_lisp()``."""

    def __init__(self, spec: s.MicrogridSpec) -> None:
        self.spec = spec

    def to_lisp(self) -> str:
        return render(self.spec)


async def _get(site: Any, path: str) -> Any:
    async with httpx.AsyncClient(base_url=f"http://{site.ui}", trust_env=False) as http:
        response = await http.get(path)
        response.raise_for_status()
        return response.json()


@pytest_asyncio.fixture
async def symbols() -> AsyncIterator[dict[str, Any]]:
    spec = s.make_microgrid(id=7, topology=s.make_grid_connection_point(id=1))
    async with mc.aio.launch(_Rendered(spec)) as site:
        listing = await _get(site, "/api/symbols")
    yield {entry["name"]: entry for entry in listing["symbols"]}


def _spec_keys(cls: type) -> set[str]:
    return {s.lisp_key(cls, name) for name in s._field_kinds(cls)}


@pytest.mark.parametrize("cls", list(PRIMITIVE), ids=lambda c: c.__name__)
async def test_spec_keys_match_the_server(cls: type, symbols: dict[str, Any]) -> None:
    server = set(symbols[PRIMITIVE[cls]]["signature"]["params"][0]["keys"])
    server -= NOT_EXPOSED.get(cls, set())
    assert _spec_keys(cls) == server


def _diamond() -> s.MicrogridSpec:
    inv = s.make_battery_inverter(successors=[s.make_battery()])
    return s.make_microgrid(
        id=7,
        topology=s.make_grid_connection_point(
            successors=[s.make_meter(successors=[inv]), s.make_meter(successors=[inv])]
        ),
    )


def _shared_battery() -> s.MicrogridSpec:
    bat = s.make_battery()
    return s.make_microgrid(
        id=7,
        topology=s.make_grid_connection_point(
            successors=[
                s.make_meter(
                    successors=[
                        s.make_battery_inverter(successors=[bat]),
                        s.make_battery_inverter(successors=[bat]),
                    ]
                )
            ]
        ),
    )


def _partial_ids() -> s.MicrogridSpec:
    return s.make_microgrid(
        id=7,
        topology=s.make_grid_connection_point(
            id=1,
            successors=[
                s.make_meter(
                    successors=[s.make_battery_inverter(successors=[s.make_battery()])]
                ),
                s.make_meter(id=1000),
                s.make_meter(),
            ],
        ),
    )


@pytest.mark.parametrize("build", [_diamond, _shared_battery, _partial_ids])
async def test_render_boots_the_spec_tree(build: Any) -> None:
    spec = build()
    async with mc.aio.launch(_Rendered(spec)) as site:
        topology = await _get(site, "/api/mg/7/topology")
    assert topology_problems(spec, topology) == []
