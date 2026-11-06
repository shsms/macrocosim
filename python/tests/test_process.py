"""spawn, the endpoints wait and the log tail, with a fake Popen."""

from __future__ import annotations

import json
import os
import subprocess
import time
from pathlib import Path
from typing import Any

import pytest

import macrocosim._process as p

pytestmark = pytest.mark.filterwarnings("error")


class FakeProcess:
    def __init__(self, returncode: int | None = None) -> None:
        self.returncode = returncode
        self.terminated = False

    def poll(self) -> int | None:
        return self.returncode

    def terminate(self) -> None:
        self.terminated = True


class PopenSpy:
    def __init__(self, error: BaseException | None = None) -> None:
        self.calls: list[tuple[list[str], dict[str, Any]]] = []
        self.error = error

    def __call__(self, args: list[str], **kwargs: Any) -> FakeProcess:
        self.calls.append((args, kwargs))
        if self.error is not None:
            raise self.error
        return FakeProcess()


@pytest.fixture
def popen(monkeypatch: pytest.MonkeyPatch) -> PopenSpy:
    spy = PopenSpy()
    monkeypatch.setattr(p.subprocess, "Popen", spy)
    return spy


def test_spawn_lays_env_over_the_environment(
    popen: PopenSpy, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("KEEP_ME", "1")
    state = tmp_path / "state"
    handle = p.spawn("/bin/mc", "(text)", env={"FAKETIME": "+60"}, state_dir=state)
    [(args, kwargs)] = popen.calls
    assert kwargs["env"] == {**os.environ, "FAKETIME": "+60"}
    assert args == [
        "/bin/mc",
        "--ephemeral-ports",
        f"--emit-endpoints={state / 'endpoints.json'}",
        f"--state-dir={state}",
        str(state / "config.lisp"),
    ]
    assert (state / "config.lisp").read_text() == "(text)"
    assert handle.log_file == state / "macrocosim.log"
    assert handle.directory == state and not handle.temporary


def _record_encodings(monkeypatch: pytest.MonkeyPatch) -> list[str | None]:
    """Record the encoding of every Path.read_text and Path.write_text call."""
    seen: list[str | None] = []
    write, read = Path.write_text, Path.read_text

    def write_text(self: Path, data: str, encoding: str | None = None, **kw: Any) -> int:
        seen.append(encoding)
        return write(self, data, encoding=encoding, **kw)

    def read_text(self: Path, encoding: str | None = None, **kw: Any) -> str:
        seen.append(encoding)
        return read(self, encoding=encoding, **kw)

    monkeypatch.setattr(Path, "write_text", write_text)
    monkeypatch.setattr(Path, "read_text", read_text)
    return seen


def test_spawn_writes_the_config_as_utf_8(
    popen: PopenSpy, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    seen = _record_encodings(monkeypatch)
    p.spawn("/bin/mc", '(name "Süd")', env=None, state_dir=tmp_path / "s")
    assert seen == ["utf-8"]
    assert (tmp_path / "s" / "config.lisp").read_bytes() == '(name "Süd")'.encode()


def test_spawn_without_env_inherits_the_environment(popen: PopenSpy) -> None:
    handle = p.spawn("/bin/mc", "x", env=None, state_dir=None)
    [(_, kwargs)] = popen.calls
    assert kwargs["env"] is None
    assert handle.temporary and handle.directory.is_dir()
    p.shutil.rmtree(handle.directory)


def test_an_absent_state_dir_is_created(popen: PopenSpy, tmp_path: Path) -> None:
    state = tmp_path / "a" / "b"
    p.spawn("/bin/mc", "x", env=None, state_dir=state)
    assert state.is_dir()


def test_an_empty_state_dir_is_used(popen: PopenSpy, tmp_path: Path) -> None:
    handle = p.spawn("/bin/mc", "x", env=None, state_dir=tmp_path)
    assert handle.directory == tmp_path


def test_a_non_empty_state_dir_raises_before_popen(
    popen: PopenSpy, tmp_path: Path
) -> None:
    (tmp_path / "enterprise.lisp").write_text("old")
    with pytest.raises(ValueError, match="absent or an empty directory"):
        p.spawn("/bin/mc", "x", env=None, state_dir=tmp_path)
    assert popen.calls == []


def test_a_failed_popen_deletes_the_temporary_dir(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    made: list[Path] = []
    real = p.tempfile.mkdtemp

    def mkdtemp(**kw: Any) -> str:
        made.append(Path(real(dir=tmp_path, **kw)))
        return str(made[-1])

    monkeypatch.setattr(p.tempfile, "mkdtemp", mkdtemp)
    monkeypatch.setattr(p.subprocess, "Popen", PopenSpy(OSError("exec format error")))
    with pytest.raises(OSError, match="exec format error"):
        p.spawn("/bin/mc", "x", env=None, state_dir=None)
    assert made and not made[0].exists()


def test_a_failed_popen_keeps_a_given_state_dir(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    monkeypatch.setattr(p.subprocess, "Popen", PopenSpy(OSError("nope")))
    with pytest.raises(OSError):
        p.spawn("/bin/mc", "x", env=None, state_dir=tmp_path / "s")
    assert (tmp_path / "s").is_dir()


def test_resolve_binary_not_found_names_every_option(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.delenv("MACROCOSIM_BIN", raising=False)
    monkeypatch.setattr(p, "which_binary", lambda name: None)
    with pytest.raises(FileNotFoundError) as info:
        p.resolve_binary("macrocosim", env_var="MACROCOSIM_BIN")
    assert str(info.value) == (
        "no macrocosim binary; install the macrocosim platform wheel, set "
        "MACROCOSIM_BIN, or put it on PATH"
    )


def test_a_bad_env_var_keeps_its_own_message(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    monkeypatch.setenv("MACROCOSIM_BIN", str(tmp_path / "missing"))
    with pytest.raises(FileNotFoundError, match="macrocosim binary not found at"):
        p.resolve_binary("macrocosim", env_var="MACROCOSIM_BIN")


def _handle(tmp_path: Path, returncode: int | None = None) -> p._Handle:
    return p._Handle(
        process=FakeProcess(returncode),  # type: ignore[arg-type]
        directory=tmp_path,
        temporary=False,
    )


def _endpoints(tmp_path: Path, microgrids: list[dict[str, Any]]) -> None:
    (tmp_path / "endpoints.json").write_text(
        json.dumps({"ui": "127.0.0.1:8801", "microgrids": microgrids})
    )


async def test_wait_endpoints_returns_the_one_microgrid(tmp_path: Path) -> None:
    _endpoints(tmp_path, [{"id": 7, "name": "x", "grpc_addr": "[::1]:4000"}])
    handle = _handle(tmp_path)
    got = await p.wait_endpoints(handle, deadline=time.monotonic() + 5)
    assert got == p.Endpoints(ui="127.0.0.1:8801", microgrid_id=7, grpc_addr="[::1]:4000")
    assert not handle.process.terminated  # type: ignore[attr-defined]


async def test_wait_endpoints_reads_the_file_as_utf_8(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    _endpoints(tmp_path, [{"id": 7, "grpc_addr": "[::1]:4000"}])
    seen = _record_encodings(monkeypatch)
    await p.wait_endpoints(_handle(tmp_path), deadline=time.monotonic() + 5)
    assert seen == ["utf-8"]


@pytest.mark.parametrize(
    "text",
    [
        "not json",
        "[1, 2]",
        '{"microgrids": 5}',
        '{"ui": "x", "microgrids": [{"id": "seven"}]}',
    ],
)
async def test_a_malformed_endpoints_file_is_its_own_error(
    tmp_path: Path, text: str
) -> None:
    (tmp_path / "endpoints.json").write_text(text)
    with pytest.raises(p._MalformedEndpoints):
        await p.wait_endpoints(_handle(tmp_path), deadline=time.monotonic() + 5)


@pytest.mark.parametrize("count", [0, 2])
async def test_wait_endpoints_needs_exactly_one_microgrid(
    tmp_path: Path, count: int
) -> None:
    _endpoints(tmp_path, [{"id": i, "grpc_addr": "x"} for i in range(1, count + 1)])
    with pytest.raises(p._NotOneMicrogrid) as info:
        await p.wait_endpoints(_handle(tmp_path), deadline=time.monotonic() + 5)
    assert info.value.count == count


async def test_wait_endpoints_reports_an_early_exit(tmp_path: Path) -> None:
    handle = _handle(tmp_path, returncode=3)
    with pytest.raises(p._ExitedEarly) as info:
        await p.wait_endpoints(handle, deadline=time.monotonic() + 5)
    assert info.value.returncode == 3
    assert not handle.process.terminated  # type: ignore[attr-defined]


async def test_wait_endpoints_times_out(tmp_path: Path) -> None:
    started = time.monotonic()
    with pytest.raises(p._EndpointsTimeout):
        await p.wait_endpoints(_handle(tmp_path), deadline=started + 0.3)
    assert time.monotonic() - started < 2


def test_log_tail(tmp_path: Path) -> None:
    handle = _handle(tmp_path)
    assert p.log_tail(handle) == ""
    handle.log_file.write_text("a" * 5000 + "END")
    tail = p.log_tail(handle)
    assert len(tail) == p.LOG_TAIL_CHARS and tail.endswith("END")


class StuckProcess:
    """A process that ignores SIGTERM, and SIGKILL too."""

    def __init__(self) -> None:
        self.calls: list[str] = []

    def poll(self) -> int | None:
        return None

    def terminate(self) -> None:
        self.calls.append("terminate")

    def kill(self) -> None:
        self.calls.append("kill")

    def wait(self, timeout: float | None = None) -> int:
        self.calls.append(f"wait {timeout}")
        raise subprocess.TimeoutExpired("mc", timeout or 0)


def test_terminate_kills_after_5_s_and_gives_up_without_raising() -> None:
    process = StuckProcess()
    p.terminate(process)  # type: ignore[arg-type]
    assert process.calls == ["terminate", "wait 5.0", "kill", "wait 5.0"]
