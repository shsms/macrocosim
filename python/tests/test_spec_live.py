"""Spec keys against the simulator's own constructors.

The boots of rendered trees are in ``test_live.py``.
"""

from __future__ import annotations

from collections.abc import AsyncIterator
from typing import Any

import pytest
import pytest_asyncio

import macrocosim as mc
import macrocosim._spec as s

pytestmark = [pytest.mark.live, pytest.mark.filterwarnings("error")]

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


@pytest_asyncio.fixture(scope="module", loop_scope="module")
async def symbols() -> AsyncIterator[dict[str, Any]]:
    """The server's symbol listing, from one simulator for the whole module."""
    spec = mc.make_microgrid(
        id=7,
        topology=mc.make_grid_connection_point(id=1, successors=[mc.make_meter(id=2)]),
    )
    async with mc.launch(spec) as microgrid:
        listing = await microgrid._session.get_json("/api/symbols")
    yield {entry["name"]: entry for entry in listing["symbols"]}


def _spec_keys(cls: type) -> set[str]:
    return {s.lisp_key(cls, name) for name in s._field_kinds(cls)}


@pytest.mark.parametrize("cls", list(PRIMITIVE), ids=lambda c: c.__name__)
async def test_spec_keys_match_the_server(cls: type, symbols: dict[str, Any]) -> None:
    server = set(symbols[PRIMITIVE[cls]]["signature"]["params"][0]["keys"])
    server -= NOT_EXPOSED.get(cls, set())
    assert _spec_keys(cls) == server
