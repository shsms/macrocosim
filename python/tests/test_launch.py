"""launch: steps, failures and teardown, with a stub process and transport."""

from __future__ import annotations

import asyncio
import contextlib
import json
import time
from collections.abc import AsyncIterator
from datetime import timedelta
from pathlib import Path
from typing import Any

import httpx
import pytest

import macrocosim._launch as launch_module
import macrocosim._spec as s
from macrocosim._errors import LaunchError, MicrogridClosed
from macrocosim._launch import _launch
from macrocosim._process import _Handle

pytestmark = pytest.mark.filterwarnings("error")

SPEC = s.make_microgrid(id=7, topology=s.make_grid_connection_point(id=1))
ONE_MG = {"ui": "127.0.0.1:8801", "microgrids": [{"id": 7, "grpc_addr": "[::1]:4000"}]}


class FakeProcess:
    def __init__(self, exits_with: int | None = None) -> None:
        self.returncode = exits_with
        self.terminated = False

    def poll(self) -> int | None:
        return self.returncode

    def terminate(self) -> None:
        self.terminated = True
        self.returncode = -15

    def wait(self, timeout: float | None = None) -> int:
        return self.returncode or 0

    def kill(self) -> None:
        self.returncode = -9


class Rig:
    """Stub binary lookup and spawn, and a mocked simulator HTTP API."""

    def __init__(self, tmp_path: Path) -> None:
        self.tmp_path = tmp_path
        self.events: list[str] = []
        self.process = FakeProcess()
        self.endpoints: dict[str, Any] | None = ONE_MG
        self.log_text = "simulator says hi"
        self.connected_after = 1
        self.status_calls = 0
        self.status_reply: int = 200
        self.graph_status: str | None = None
        self.handle: _Handle | None = None
        self.spawn_kwargs: dict[str, Any] = {}
        self.exit_on_status = False
        self.exit_on_topology = False
        self.status_body: Any = None

    def find_binary(self) -> str:
        self.events.append("find")
        return "/bin/macrocosim"

    def spawn(
        self, binary: str, text: str, *, env: Any, state_dir: Path | None
    ) -> _Handle:
        self.events.append("spawn")
        self.spawn_kwargs = {"binary": binary, "env": env, "state_dir": state_dir}
        assert text.startswith("(make-microgrid")
        if state_dir is None:
            directory, temporary = self.tmp_path / "tmp-run", True
        else:
            directory, temporary = state_dir, False
        directory.mkdir(parents=True, exist_ok=True)
        (directory / "macrocosim.log").write_text(self.log_text)
        if self.endpoints is not None:
            (directory / "endpoints.json").write_text(json.dumps(self.endpoints))
        self.handle = _Handle(
            process=self.process,  # type: ignore[arg-type]
            directory=directory,
            temporary=temporary,
        )
        return self.handle

    def handler(self, request: httpx.Request) -> httpx.Response:
        path = request.url.path
        self.events.append(f"GET {path}")
        if path.endswith("/metrics/status"):
            self.status_calls += 1
            if self.exit_on_status:
                self.process.returncode = 3
                raise httpx.ConnectError("connection refused", request=request)
            if self.status_reply != 200:
                return httpx.Response(self.status_reply, json={"error": "boom"})
            if self.status_body is not None:
                return httpx.Response(200, json=self.status_body)
            connected = self.status_calls >= self.connected_after
            return httpx.Response(200, json={"connected": connected})
        if path.endswith("/topology"):
            if self.exit_on_topology:
                self.process.returncode = 3
                raise httpx.ConnectError("connection refused", request=request)
            return httpx.Response(200, json={"graph_status": self.graph_status})
        return httpx.Response(404, json={"error": f"no route for GET {path}"})

    def launch(self, **kw: Any) -> Any:
        kw.setdefault("ready_timeout", timedelta(seconds=2))
        return _launch(
            SPEC,
            _find_binary=self.find_binary,
            _spawn=self.spawn,
            _transport=httpx.MockTransport(self.handler),
            **kw,
        )


@pytest.fixture
def rig(tmp_path: Path) -> Rig:
    return Rig(tmp_path)


async def test_steps_run_in_order_and_teardown_cleans_up(rig: Rig) -> None:
    rig.connected_after = 2
    async with rig.launch(env={"FAKETIME": "+60"}) as mg:
        assert mg.id == 7
        assert mg.grpc_url == "grpc://[::1]:4000"
        assert mg.ui_url == "http://127.0.0.1:8801"
        assert mg.log_path is None
        assert rig.events == [
            "find",
            "spawn",
            "GET /api/mg/7/metrics/status",
            "GET /api/mg/7/metrics/status",
        ]
        assert rig.spawn_kwargs["env"] == {"FAKETIME": "+60"}
    assert rig.process.terminated
    assert rig.handle is not None and not rig.handle.directory.exists()
    with pytest.raises(MicrogridClosed):
        await mg.grid_power.read()


async def test_step1_no_binary(rig: Rig) -> None:
    def missing() -> str:
        raise FileNotFoundError("no macrocosim binary; install ...")

    rig.find_binary = missing  # type: ignore[method-assign]
    with pytest.raises(LaunchError) as info:
        async with rig.launch():
            pass
    assert str(info.value) == "no macrocosim binary; install ..."
    assert info.value.log_tail == "" and info.value.log_path is None
    assert "spawn" not in rig.events


async def test_steps_2_to_4_os_error(rig: Rig) -> None:
    def broken(*a: Any, **k: Any) -> _Handle:
        raise PermissionError("permission denied: /bin/macrocosim")

    rig.spawn = broken  # type: ignore[method-assign]
    with pytest.raises(LaunchError) as info:
        async with rig.launch():
            pass
    assert str(info.value) == (
        "could not start the simulator: permission denied: /bin/macrocosim"
    )
    assert info.value.log_tail == ""


async def test_a_non_empty_state_dir_value_error_propagates(rig: Rig) -> None:
    def refuses(*a: Any, **k: Any) -> _Handle:
        raise ValueError("state_dir must be absent or an empty directory: /x")

    rig.spawn = refuses  # type: ignore[method-assign]
    with pytest.raises(ValueError, match="absent or an empty directory"):
        async with rig.launch(state_dir="/x"):
            pass


async def test_step5_process_exits_early(rig: Rig) -> None:
    rig.endpoints = None
    rig.process = FakeProcess(exits_with=3)
    rig.log_text = "panic: bad config"
    with pytest.raises(LaunchError) as info:
        async with rig.launch():
            pass
    assert str(info.value) == (
        "the simulator exited with code 3 before writing its endpoints\n"
        "panic: bad config\n"
        "  Pass state_dir to launch to keep the simulator log."
    )
    assert info.value.log_tail == "panic: bad config"
    assert rig.handle is not None and not rig.handle.directory.exists()


async def test_step5_timeout_keeps_a_given_state_dir_and_names_its_log(
    rig: Rig, tmp_path: Path
) -> None:
    rig.endpoints = None
    state = tmp_path / "state"
    with pytest.raises(LaunchError) as info:
        async with rig.launch(state_dir=state, ready_timeout=timedelta(seconds=0.3)):
            pass
    assert str(info.value) == (
        "the simulator wrote no endpoints within 0.3 s\n"
        "simulator says hi\n"
        f"  Simulator log: {state / 'macrocosim.log'}"
    )
    assert info.value.log_path == state / "macrocosim.log"
    assert rig.process.terminated
    assert state.is_dir()


async def test_step5_not_one_microgrid(rig: Rig) -> None:
    rig.endpoints = {"ui": "x", "microgrids": []}
    with pytest.raises(LaunchError, match="listed 0 microgrids in its endpoints file"):
        async with rig.launch():
            pass
    assert rig.process.terminated


async def test_step5_malformed_endpoints(rig: Rig) -> None:
    rig.endpoints = {"ui": "x", "microgrids": [{"id": "seven", "grpc_addr": "y"}]}
    with pytest.raises(LaunchError, match="endpoints file is not what the client"):
        async with rig.launch():
            pass
    assert rig.process.terminated


async def test_step6_status_of_the_wrong_shape(rig: Rig) -> None:
    rig.status_body = [1, 2]
    with pytest.raises(LaunchError, match="could not check .* did not expect"):
        async with rig.launch():
            pass
    assert rig.process.terminated


async def test_an_exit_at_the_deadline_is_not_a_timeout(rig: Rig) -> None:
    rig.connected_after = 10**6
    rig.exit_on_topology = True
    with pytest.raises(LaunchError, match="exited with code 3 before its loopback"):
        async with rig.launch(ready_timeout=timedelta(seconds=0.3)):
            pass


async def test_step6_server_error(rig: Rig) -> None:
    rig.status_reply = 500
    with pytest.raises(LaunchError) as info:
        async with rig.launch():
            pass
    assert str(info.value).startswith(
        "could not check the simulator's loopback: GET /api/mg/7/metrics/status "
        "answered 500: boom\nsimulator says hi"
    )
    assert rig.process.terminated


async def test_step6_timeout_reports_graph_status(rig: Rig) -> None:
    rig.connected_after = 10**6
    rig.graph_status = "no grid meter"
    with pytest.raises(LaunchError) as info:
        async with rig.launch(ready_timeout=timedelta(seconds=0.6)):
            pass
    assert str(info.value).startswith(
        "the simulator's loopback did not connect within 0.6 s; "
        "graph_status: no grid meter\n"
    )
    assert rig.process.terminated


async def test_one_deadline_covers_steps_5_and_6(rig: Rig) -> None:
    rig.endpoints = None
    rig.connected_after = 10**6

    async def late_endpoints() -> None:
        await asyncio.sleep(0.4)
        assert rig.handle is not None
        (rig.handle.directory / "endpoints.json").write_text(json.dumps(ONE_MG))

    writer = asyncio.create_task(late_endpoints())
    started = time.monotonic()
    with pytest.raises(LaunchError, match="did not connect within 0.6 s"):
        async with rig.launch(ready_timeout=timedelta(seconds=0.6)):
            pass
    await writer
    assert time.monotonic() - started < 1.2
    assert rig.status_calls <= 2  # 0.25 s apart, in what the endpoints wait left


async def test_cancellation_during_launch_stops_the_process(rig: Rig) -> None:
    rig.endpoints = None

    async def body() -> None:
        async with rig.launch(ready_timeout=timedelta(seconds=30)):
            pass

    task = asyncio.create_task(body())
    await asyncio.sleep(0.3)
    task.cancel()
    with pytest.raises(asyncio.CancelledError):
        await task
    assert rig.process.terminated
    assert rig.handle is not None and not rig.handle.directory.exists()


async def test_cancellation_in_the_body_stops_the_process(rig: Rig) -> None:
    async def body() -> None:
        async with rig.launch():
            await asyncio.sleep(30)

    task = asyncio.create_task(body())
    await asyncio.sleep(0.2)
    task.cancel()
    with pytest.raises(asyncio.CancelledError):
        await task
    assert rig.process.terminated


async def test_a_body_exception_still_deletes_a_temporary_dir(rig: Rig) -> None:
    with pytest.raises(RuntimeError, match="test failed"):
        async with rig.launch():
            raise RuntimeError("test failed")
    assert rig.handle is not None and not rig.handle.directory.exists()
    assert rig.process.terminated


async def test_a_given_state_dir_is_kept_after_exit_and_failure(
    rig: Rig, tmp_path: Path
) -> None:
    state = tmp_path / "kept"
    async with rig.launch(state_dir=state) as mg:
        assert mg.log_path == state / "macrocosim.log"
    assert state.is_dir()
    state2 = tmp_path / "kept2"
    with pytest.raises(RuntimeError):
        async with rig.launch(state_dir=state2):
            raise RuntimeError("x")
    assert state2.is_dir()


async def test_an_exit_during_the_loopback_wait_names_the_exit_once(rig: Rig) -> None:
    rig.exit_on_status = True
    with pytest.raises(LaunchError) as info:
        async with rig.launch():
            pass
    assert str(info.value) == (
        "the simulator exited with code 3 before its loopback connected"
        "\nsimulator says hi"
        "\n  Pass state_dir to launch to keep the simulator log."
    )


async def test_launch_forwards_its_arguments(monkeypatch: pytest.MonkeyPatch) -> None:
    seen: dict[str, Any] = {}

    @contextlib.asynccontextmanager
    async def fake(spec: Any, **kw: Any) -> AsyncIterator[str]:
        seen.update(kw, spec=spec)
        yield "microgrid"

    monkeypatch.setattr(launch_module, "_launch", fake)
    timeout = timedelta(seconds=3)
    async with launch_module.launch(
        SPEC, env={"A": "1"}, state_dir="/x", ready_timeout=timeout
    ) as got:
        assert got == "microgrid"
    assert seen == {
        "spec": SPEC,
        "env": {"A": "1"},
        "state_dir": "/x",
        "ready_timeout": timeout,
    }
