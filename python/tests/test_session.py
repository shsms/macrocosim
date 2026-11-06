"""The HTTP session: status mapping, the closed check, and process exits."""

from __future__ import annotations

from pathlib import Path

import httpx
import pytest

from macrocosim._errors import (
    MicrogridClosed,
    Rejected,
    ServerError,
    SimulatorExited,
)
from macrocosim._session import _Session

pytestmark = pytest.mark.filterwarnings("error")


def _session(handler, exit_probe=lambda: None, log_path=None) -> _Session:
    return _Session(
        "http://sim",
        7,
        transport=httpx.MockTransport(handler),
        exit_probe=exit_probe,
        log_path=log_path,
    )


async def test_get_json_returns_the_decoded_body() -> None:
    s = _session(lambda r: httpx.Response(200, json={"a": 1}))
    assert await s.get_json("/api/mg/7/topology") == {"a": 1}
    await s.close()


async def test_post_json_returns_none_for_204() -> None:
    seen: list[str] = []

    def handler(request: httpx.Request) -> httpx.Response:
        seen.append(request.url.path)
        return httpx.Response(204)

    s = _session(handler)
    drive = "/api/mg/7/component/1/drive"
    status = "/api/mg/7/component/1/status"
    assert (
        await s.post_json(drive, {"power_w": 1.0}, call="Meter 1 power.set(1 W)") is None
    )
    assert (
        await s.post_json(status, {"health": "ok"}, call="Meter 1 health.set(ok)") is None
    )
    assert seen == [drive, status]
    await s.close()


@pytest.mark.parametrize("status", [400, 409])
@pytest.mark.parametrize("action", ["drive", "status"])
async def test_rejected_control_raises(status: int, action: str) -> None:
    s = _session(
        lambda r: httpx.Response(status, json={"error": "component 9 not found"})
    )
    with pytest.raises(Rejected) as info:
        await s.post_json(
            f"/api/mg/7/component/9/{action}", {}, call="Meter 9 power.set(1 W)"
        )
    assert str(info.value) == "Meter 9 power.set(1 W) was rejected: component 9 not found"
    assert info.value.text == "component 9 not found"
    assert not isinstance(info.value, ValueError)
    await s.close()


@pytest.mark.parametrize(
    ("status", "path"),
    [
        (400, "/api/mg/7/component/1/history"),
        (404, "/api/mg/7/component/1/drive"),
        (422, "/api/mg/7/component/1/status"),
        (403, "/api/mg/7/component/1/drive"),
        (500, "/api/mg/7/metrics/latest"),
    ],
)
async def test_other_failures_raise_server_error(status: int, path: str) -> None:
    s = _session(lambda r: httpx.Response(status, json={"error": "nope"}))
    with pytest.raises(ServerError) as info:
        if path.endswith(("drive", "status")):
            await s.post_json(path, {}, call="x")
        else:
            await s.get_json(path)
    assert not isinstance(info.value, (Rejected, SimulatorExited))
    assert info.value.status == status
    assert str(info.value).endswith(f"{path} answered {status}: nope")
    await s.close()


async def test_a_non_json_error_body_is_quoted_as_text() -> None:
    s = _session(lambda r: httpx.Response(502, text="bad gateway"))
    with pytest.raises(ServerError, match="answered 502: bad gateway"):
        await s.get_json("/x")
    await s.close()


def _refuse(request: httpx.Request) -> httpx.Response:
    raise httpx.ConnectError("connection refused", request=request)


async def test_a_transport_failure_with_the_process_alive_is_a_server_error() -> None:
    s = _session(_refuse)
    with pytest.raises(ServerError) as info:
        await s.get_json("/api/mg/7/metrics/latest")
    assert type(info.value) is ServerError
    assert str(info.value) == "GET /api/mg/7/metrics/latest failed: connection refused"
    await s.close()


async def test_a_transport_failure_after_the_process_exited() -> None:
    log = Path("/tmp/state/macrocosim.log")
    s = _session(_refuse, exit_probe=lambda: (3, "panic: boom"), log_path=log)
    with pytest.raises(SimulatorExited) as info:
        await s.get_json("/api/mg/7/metrics/latest")
    assert info.value.returncode == 3
    assert str(info.value) == (
        "the simulator exited with code 3 during GET /api/mg/7/metrics/latest\n"
        "panic: boom\n"
        f"  Simulator log: {log}"
    )
    await s.close()


async def test_a_request_the_teardown_breaks_raises_microgrid_closed() -> None:
    def handler(request: httpx.Request) -> httpx.Response:
        s.mark_closed()  # the launch block exits while this request runs
        raise httpx.ReadError("connection reset", request=request)

    s = _session(handler, exit_probe=lambda: (-15, ""))
    with pytest.raises(MicrogridClosed, match="closed during GET /api/mg/7/topology"):
        await s.get_json("/api/mg/7/topology")
    await s.close()


async def test_a_transport_error_without_text_is_named_by_its_type() -> None:
    def handler(request: httpx.Request) -> httpx.Response:
        raise httpx.ReadTimeout("", request=request)

    s = _session(handler)
    with pytest.raises(ServerError, match="^GET /x failed: ReadTimeout$"):
        await s.get_json("/x")
    await s.close()


def test_microgrid_path_puts_a_route_under_the_microgrid() -> None:
    s = _session(lambda r: httpx.Response(204))
    assert s.microgrid_path("metrics/latest") == "/api/mg/7/metrics/latest"


async def test_get_json_object_refuses_an_answer_that_is_not_an_object() -> None:
    s = _session(lambda r: httpx.Response(200, json=[1, 2]))
    with pytest.raises(ServerError, match="did not expect"):
        await s.get_json_object("/api/mg/7/topology")
    await s.close()


async def test_calls_after_close_raise_microgrid_closed_before_any_io() -> None:
    calls: list[httpx.Request] = []

    def handler(request: httpx.Request) -> httpx.Response:
        calls.append(request)
        return httpx.Response(200, json={})

    s = _session(handler)
    await s.close()
    with pytest.raises(MicrogridClosed, match="microgrid 7 is closed: its launch block"):
        await s.get_json("/x")
    with pytest.raises(MicrogridClosed):
        await s.post_json("/x/drive", {}, call="x")
    assert calls == []


async def test_a_success_whose_body_is_not_json_is_a_server_error() -> None:
    s = _session(lambda r: httpx.Response(200, text="<html>"))
    with pytest.raises(ServerError, match="answered 200 with a body that is not JSON"):
        await s.get_json("/x")
    await s.close()
