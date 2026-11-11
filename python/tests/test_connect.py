"""connect: attach to a running simulator, and declare microgrids on it."""

from __future__ import annotations

import asyncio
import json
from collections.abc import Callable
from pathlib import Path
from typing import Any

import httpx
import pytest
from frequenz.quantities import Percentage, Power

import macrocosim as mc
from macrocosim._connect import DEFAULT_URL, _connect, _resolve_url
from macrocosim._process import client_version
from macrocosim._render import render_microgrid

pytestmark = pytest.mark.filterwarnings("error")


def _spec(mg_id: int, meter_id: int) -> mc.MicrogridSpec:
    return mc.make_microgrid(
        id=mg_id,
        topology=mc.make_grid_connection_point(
            id=meter_id - 1, successors=[mc.make_meter(id=meter_id)]
        ),
    )


def _auto(mg_id: int, meters: int = 1) -> mc.MicrogridSpec:
    """Microgrid `mg_id` whose components all have automatic ids."""
    return mc.make_microgrid(
        id=mg_id,
        topology=mc.make_grid_connection_point(
            successors=[mc.make_meter() for _ in range(meters)]
        ),
    )


class _Engine:
    """A simulator with three microgrids, which takes declarations."""

    def __init__(self) -> None:
        self.specs = {7: _spec(7, 2), 8: _spec(8, 12), 9: _spec(9, 22)}
        self.paths: list[str] = []
        self.spec_route = True
        self.unloaded: set[int] = set()
        self.listings = 0
        self.declarations: list[tuple[dict[str, str], dict[str, Any]]] = []
        self.answer: tuple[int, Any] | None = None
        self.runtime: dict[str, Any] = {"status": "running", "grpc_addr": "[::1]:9000"}

    def __call__(self, request: httpx.Request) -> httpx.Response:
        path = request.url.path
        self.paths.append(f"{request.method} {path}")
        if path == "/api/microgrids" and request.method == "POST":
            body = json.loads(request.content)
            self.declarations.append((dict(request.url.params), body))
            if self.answer is not None:
                code, answer = self.answer
                return httpx.Response(code, json=answer)
            entries = [
                {
                    "id": mg["id"],
                    "grpc_port": 9000,
                    "runtime": self.runtime,
                    "outcome": "created",
                }
                for mg in body["microgrids"]
            ]
            return httpx.Response(200, json={"microgrids": entries})
        if path == "/api/microgrids":
            entries: list[dict[str, Any]] = [
                {"id": 7, "runtime": {"status": "running", "grpc_addr": "[::1]:8800"}},
                {"id": 8, "runtime": None},
                # A server that ended keeps its address in the listing.
                {"id": 9, "runtime": {"status": "failed", "grpc_addr": "[::1]:8810"}},
            ]
            # The microgrids in `unloaded` go after the first two listings
            # (entering the block, then `microgrids()`).
            if self.listings > 1:
                entries = [e for e in entries if e["id"] not in self.unloaded]
            self.listings += 1
            return httpx.Response(200, json=entries)
        for mg_id in (7, 8, 9):
            if path == f"/api/mg/{mg_id}/spec" and self.spec_route:
                if mg_id in self.unloaded:
                    error = f"microgrid {mg_id} not registered"
                    return httpx.Response(404, json={"error": error})
                return httpx.Response(200, json=render_microgrid(self.specs[mg_id]))
        if request.method == "DELETE" and path == "/api/mg/7":
            return httpx.Response(204)
        if request.method == "POST":
            return httpx.Response(204)
        return httpx.Response(
            404, json={"error": f"no route for {request.method} {path}"}
        )

    def ids(self, declaration: int, mg: int = 0) -> list[int]:
        """The component ids of the `mg`-th microgrid of the `declaration`-th
        declaration received, both counting from 0."""
        doc = self.declarations[declaration][1]
        return sorted(c["id"] for c in doc["microgrids"][mg]["components"])


def _open(handler: Callable[[httpx.Request], Any]) -> Any:
    """A connection whose requests `handler` answers, such as an `_Engine`."""
    return _connect("http://127.0.0.1:8801/", _transport=httpx.MockTransport(handler))


async def test_microgrids_lists_each_microgrid() -> None:
    engine = _Engine()
    async with _open(engine) as sim:
        microgrids = await sim.microgrids()
        assert sorted(microgrids) == [7, 8, 9]
        seven = microgrids[7]
        assert seven.id == 7
        assert seven.grpc_url == "grpc://[::1]:8800"
        assert seven.ui_url == "http://127.0.0.1:8801"
        for idle in (8, 9):
            with pytest.raises(mc.LaunchError, match=f"{idle} had no running gRPC"):
                _ = microgrids[idle].grpc_url
        assert repr(seven).startswith("Microgrid 7 at grpc://[::1]:8800")
        assert repr(microgrids[8]).startswith("Microgrid 8 at no running gRPC server")
        assert 2 in seven and 12 in microgrids[8] and 12 not in seven
        await seven[2].power.set(Power.from_kilowatts(3))  # type: ignore[attr-defined]
    assert "POST /api/mg/7/component/2/drive" in engine.paths
    with pytest.raises(mc.MicrogridClosed):
        await seven[2].power.set(Power.from_kilowatts(3))  # type: ignore[attr-defined]


async def test_a_microgrid_unloaded_since_the_listing_is_left_out() -> None:
    engine = _Engine()
    engine.unloaded.add(8)
    async with _open(engine) as sim:
        assert sorted(await sim.microgrids()) == [7, 9]


async def test_a_missing_spec_route_is_an_error() -> None:
    engine = _Engine()
    engine.spec_route = False
    async with _open(engine) as sim:
        with pytest.raises(mc.ServerError, match="no route"):
            await sim.microgrids()


@pytest.mark.parametrize("url", ["127.0.0.1:8801", "grpc://[::1]:8800", "http://"])
async def test_connect_takes_an_http_url(url: str) -> None:
    with pytest.raises(ValueError, match="connect takes the simulator's UI address"):
        async with mc.connect(url):
            pass


def test_the_url_comes_from_the_argument_the_environment_or_the_default(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.delenv("MACROCOSIM_URL", raising=False)
    assert _resolve_url(None) == DEFAULT_URL == "http://127.0.0.1:8801"
    monkeypatch.setenv("MACROCOSIM_URL", "http://127.0.0.1:9901")
    assert _resolve_url(None) == "http://127.0.0.1:9901"
    assert _resolve_url("http://[::1]:7000") == "http://[::1]:7000"


async def test_declare_sends_a_declaration_and_returns_its_microgrids() -> None:
    engine = _Engine()
    meter = mc.make_meter(id=31)
    site = mc.make_microgrid(
        id=30, topology=mc.make_grid_connection_point(id=30, successors=[meter])
    )
    async with _open(engine) as sim:
        declared = await sim.declare(site)
        assert isinstance(declared, mc.Microgrid)
        assert declared.id == 30
        assert declared.grpc_url == "grpc://[::1]:9000"
        await declared[meter].power.set(Power.from_kilowatts(1))  # type: ignore[attr-defined]
        pair = await sim.declare(_spec(40, 42), _spec(50, 52))
        assert [m.id for m in pair] == [40, 50]
    params, _ = engine.declarations[0]
    assert params == {"existing": "rebuild", "client_version": client_version()}
    assert "POST /api/mg/30/component/31/drive" in engine.paths
    assert len(engine.declarations[1][1]["microgrids"]) == 2, "one request"


async def test_declare_checks_its_specs_before_any_request() -> None:
    engine = _Engine()
    async with _open(engine) as sim:
        with pytest.raises(TypeError, match="at least one"):
            await sim.declare()  # type: ignore[call-overload]
        no_id = mc.make_microgrid(topology=mc.make_grid_connection_point(id=1))
        with pytest.raises(ValueError, match="no microgrid id"):
            await sim.declare(no_id)
        with pytest.raises(mc.SpecError, match="unique across the microgrids"):
            await sim.declare(_spec(40, 42), _spec(50, 42))
    assert engine.declarations == []


async def test_automatic_ids_continue_across_declarations() -> None:
    engine = _Engine()
    explicit = _spec(20, 1001)  # ids 1000 and 1001
    async with _open(engine) as sim:
        await sim.declare(explicit)
        await sim.declare(_auto(21))
        await sim.declare(_auto(22, meters=2))
        await sim.declare(_auto(21))
        await sim.declare(_auto(21, meters=2))
    assert engine.ids(0) == [1000, 1001]
    assert engine.ids(1) == [1002, 1003], "past the explicit microgrid's ids"
    assert engine.ids(2) == [1004, 1005, 1006]
    assert engine.ids(3) == [1002, 1003], "the same ids again"
    assert engine.ids(4) == [1002, 1003, 1007], "grown: skips microgrid 22's ids"
    async with _open(engine) as sim:
        await sim.declare(_auto(21))
    assert engine.ids(5) == [1000, 1001], "a new connection starts again"


async def test_declare_leaves_the_callers_spec_as_it_was() -> None:
    engine = _Engine()
    spec = _auto(21)
    before = dict(spec._ids)
    async with _open(engine) as sim:
        await sim.declare(_spec(20, 1001))
        declared = await sim.declare(spec)
        meter = spec.topology.successors[0]
        assert declared[meter].id == 1002, "children first, past 1000 and 1001"
    assert spec._ids == before


@pytest.mark.parametrize("code", [400, 409])
async def test_a_refused_declaration_raises_rejected(code: int) -> None:
    engine = _Engine()
    engine.answer = (code, {"error": "component id 2 is already in microgrid 7"})
    async with _open(engine) as sim:
        with pytest.raises(mc.Rejected, match="already in microgrid 7"):
            await sim.declare(_spec(20, 2))


async def test_an_invalid_declaration_raises_server_error() -> None:
    engine = _Engine()
    engine.answer = (422, {"error": "microgrid 20: a bad spec"})
    async with _open(engine) as sim:
        with pytest.raises(mc.ServerError, match="a bad spec"):
            await sim.declare(_spec(20, 2))


async def test_an_answer_without_outcomes_raises_server_error() -> None:
    engine = _Engine()
    entry = {"id": 20, "grpc_port": 9000, "runtime": None}
    engine.answer = (201, {"microgrids": [entry]})
    async with _open(engine) as sim:
        with pytest.raises(mc.ServerError, match="do the client and the simulator"):
            await sim.declare(_spec(20, 2))


async def test_a_failed_runtime_raises_server_error() -> None:
    engine = _Engine()
    engine.runtime = {"status": "failed", "grpc_addr": None, "error": "port taken"}
    async with _open(engine) as sim:
        with pytest.raises(mc.ServerError, match="microgrid 20: port taken"):
            await sim.declare(_spec(20, 2))


async def test_a_closed_connection_raises_connection_closed() -> None:
    engine = _Engine()
    async with _open(engine) as sim:
        pass
    calls = len(engine.paths)
    with pytest.raises(
        mc.ConnectionClosed, match="the connection is closed: its block has exited"
    ):
        await sim.declare(_spec(20, 2))
    with pytest.raises(mc.ConnectionClosed):
        await sim.unload(7)
    with pytest.raises(mc.ConnectionClosed):
        await sim.microgrids()
    # An answer that arrives after the block exited opens no session.
    with pytest.raises(mc.ConnectionClosed):
        sim._session_for(30)
    assert len(engine.paths) == calls, "no request"


async def test_a_call_in_flight_when_the_block_exits_raises_connection_closed() -> None:
    engine = _Engine()
    arrived, release = asyncio.Event(), asyncio.Event()

    async def handler(request: httpx.Request) -> httpx.Response:
        if request.method == "POST":
            arrived.set()
            await release.wait()
            raise httpx.ReadError("closed", request=request)
        return engine(request)

    async with _open(handler) as sim:
        declaring = asyncio.create_task(sim.declare(_spec(20, 2)))
        waiting = asyncio.create_task(arrived.wait())
        await asyncio.wait({declaring, waiting}, return_when=asyncio.FIRST_COMPLETED)
        assert not declaring.done(), declaring.exception()
    release.set()
    with pytest.raises(
        mc.ConnectionClosed, match="the connection was closed during POST /api/micro"
    ):
        await declaring


async def test_a_listing_in_flight_when_the_block_exits_raises_connection_closed() -> (
    None
):
    engine = _Engine()
    arrived, release = asyncio.Event(), asyncio.Event()

    async def handler(request: httpx.Request) -> httpx.Response:
        if request.url.path == "/api/mg/7/spec":
            arrived.set()
            await release.wait()
            raise httpx.ReadError("closed", request=request)
        return engine(request)

    async with _open(handler) as sim:
        listing = asyncio.create_task(sim.microgrids())
        waiting = asyncio.create_task(arrived.wait())
        await asyncio.wait({listing, waiting}, return_when=asyncio.FIRST_COMPLETED)
        assert not listing.done(), listing.exception()
    release.set()
    with pytest.raises(
        mc.ConnectionClosed, match="the connection was closed during GET /api/mg/7/"
    ):
        await listing


async def test_unload_deletes_the_microgrid() -> None:
    engine = _Engine()
    async with _open(engine) as sim:
        await sim.unload(7)
        with pytest.raises(mc.ServerError, match="no route"):
            await sim.unload(70)
    assert "DELETE /api/mg/7" in engine.paths


@pytest.mark.live
async def test_live_connect_declares_on_the_launched_simulator(tmp_path: Path) -> None:
    battery = mc.make_battery(id=3, initial_soc=Percentage.from_percent(50))
    inverter = mc.make_battery_inverter(id=2, successors=[battery])

    def site(meter: bool) -> mc.MicrogridSpec:
        successors: list[mc.ComponentSpec[Any]] = [inverter]
        if meter:
            successors.append(mc.make_meter(id=4))
        return mc.make_microgrid(
            id=7, topology=mc.make_grid_connection_point(id=1, successors=successors)
        )

    eighty = mc.between(Percentage.from_percent(79), Percentage.from_percent(81))
    async with mc.launch(site(False), state_dir=tmp_path / "mc") as launched:
        async with mc.connect(launched.ui_url) as sim:
            [attached] = (await sim.microgrids()).values()
            assert attached.grpc_url == launched.grpc_url
            same = await sim.declare(site(False))
            await same[battery].soc.set(Percentage.from_percent(80))  # type: ignore[attr-defined]
            grown = await sim.declare(site(True))
            assert 4 in grown
            await grown[battery].soc.expect(eighty)  # type: ignore[attr-defined]
