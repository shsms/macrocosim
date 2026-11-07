"""connect: attach to the microgrids of a running simulator."""

from __future__ import annotations

from pathlib import Path
from typing import Any

import httpx
import pytest
from frequenz.quantities import Power

import macrocosim as mc
from macrocosim._connect import _connect
from macrocosim._render import render_microgrid

pytestmark = pytest.mark.filterwarnings("error")


def _spec(mg_id: int, meter_id: int) -> mc.MicrogridSpec:
    return mc.make_microgrid(
        id=mg_id,
        topology=mc.make_grid_connection_point(
            id=meter_id - 1, successors=[mc.make_meter(id=meter_id)]
        ),
    )


class _Engine:
    """A simulator with two spec microgrids and one loaded from Lisp."""

    def __init__(self) -> None:
        self.specs = {7: _spec(7, 2), 8: _spec(8, 12)}
        self.paths: list[str] = []

    def __call__(self, request: httpx.Request) -> httpx.Response:
        path = request.url.path
        self.paths.append(f"{request.method} {path}")
        if path == "/api/microgrids":
            entries: list[dict[str, Any]] = [
                {"id": 7, "has_spec": True, "runtime": {"grpc_addr": "[::1]:8800"}},
                {"id": 8, "has_spec": True, "runtime": None},
                {"id": 9, "has_spec": False, "runtime": {"grpc_addr": "[::1]:8820"}},
            ]
            return httpx.Response(200, json=entries)
        for mg_id, spec in self.specs.items():
            if path == f"/api/mg/{mg_id}/spec":
                return httpx.Response(200, json=render_microgrid(spec))
        if request.method == "POST":
            return httpx.Response(204)
        return httpx.Response(404, json={"error": "no route"})


async def test_connect_yields_each_microgrid_with_a_spec() -> None:
    engine = _Engine()
    async with _connect(
        "http://127.0.0.1:8801/", _transport=httpx.MockTransport(engine)
    ) as microgrids:
        assert sorted(microgrids) == [7, 8]
        seven = microgrids[7]
        assert seven.id == 7
        assert seven.grpc_url == "grpc://[::1]:8800"
        assert seven.ui_url == "http://127.0.0.1:8801"
        assert microgrids[8].grpc_url == "grpc://"
        assert 2 in seven and 12 in microgrids[8] and 12 not in seven
        await seven[2].power.set(Power.from_kilowatts(3))  # type: ignore[attr-defined]
    assert "POST /api/mg/7/component/2/drive" in engine.paths
    assert "GET /api/mg/9/spec" not in engine.paths
    with pytest.raises(mc.MicrogridClosed):
        await seven[2].power.set(Power.from_kilowatts(3))  # type: ignore[attr-defined]


@pytest.mark.parametrize("url", ["127.0.0.1:8801", "grpc://[::1]:8800", "http://"])
async def test_connect_takes_an_http_url(url: str) -> None:
    with pytest.raises(ValueError, match="connect takes the simulator's UI address"):
        async with mc.connect(url):
            pass


@pytest.mark.live
async def test_live_connect_drives_the_launched_microgrid(tmp_path: Path) -> None:
    load = mc.make_meter(id=2)
    spec = mc.make_microgrid(
        id=7, topology=mc.make_grid_connection_point(id=1, successors=[load])
    )
    async with mc.launch(spec, state_dir=tmp_path / "mc") as launched:
        async with mc.connect(launched.ui_url) as microgrids:
            [attached] = microgrids.values()
            assert attached.id == 7
            assert attached.grpc_url == launched.grpc_url
            assert isinstance(attached[2], mc.Meter)
            await attached[2].power.set(Power.from_kilowatts(4))  # type: ignore[attr-defined]
        await launched[load].power.expect(
            mc.near(Power.from_kilowatts(4), tol=Power.from_watts(1))
        )
