"""The script child (`python -m macrocosim._runner`) against a fake engine."""

from __future__ import annotations

import json
import os
import subprocess
import sys
import textwrap
from pathlib import Path
from typing import Any

import httpx
import pytest

from macrocosim._process import client_version
from macrocosim._runner import (
    EXIT_FINISHED,
    EXIT_NOT_LOADED,
    EXIT_RAISED,
    run_script,
)

pytestmark = pytest.mark.filterwarnings("error")

URL = "http://127.0.0.1:8801"


@pytest.fixture(autouse=True)
def _keep_main(monkeypatch: pytest.MonkeyPatch) -> None:
    """Put back the ``__main__`` module and ``sys.argv`` a script run here
    replaces, as it does in the child it was written for."""
    monkeypatch.setattr(sys, "argv", list(sys.argv))
    monkeypatch.setitem(sys.modules, "__main__", sys.modules["__main__"])


class _Engine:
    """Answers the hello with `answer` (each microgrid's gRPC address, or the
    status and error), and records every request."""

    def __init__(self, status: int = 200, error: str = "") -> None:
        self.status = status
        self.error = error
        self.requests: list[tuple[str, str, Any]] = []

    def __call__(self, request: httpx.Request) -> httpx.Response:
        body = json.loads(request.content) if request.content else None
        self.requests.append((request.method, request.url.path, body))
        if request.url.path == "/api/runner/hello":
            if self.status != 200:
                return httpx.Response(self.status, json={"error": self.error})
            ids = [m["id"] for m in body["topology"]["microgrids"]]
            return httpx.Response(
                200,
                json={
                    "microgrids": [
                        {"id": i, "grpc_addr": f"[::1]:{8800 + n}"}
                        for n, i in enumerate(ids)
                    ]
                },
            )
        return httpx.Response(204)

    @property
    def hello(self) -> Any:
        [body] = [b for _, p, b in self.requests if p == "/api/runner/hello"]
        return body


def _script(tmp_path: Path, body: str, name: str = "plant.py") -> Path:
    path = tmp_path / name
    path.write_text(textwrap.dedent(body))
    return path


async def _run(path: Path, engine: _Engine) -> int:
    return await run_script(path, URL, "tok", transport=httpx.MockTransport(engine))


TWO = """
    import macrocosim as mc
    from frequenz.quantities import Power

    load = mc.make_meter()
    MICROGRIDS = [
        mc.make_microgrid(
            id=1, topology=mc.make_grid_connection_point(successors=[load])
        ),
        mc.make_microgrid(
            id=2, topology=mc.make_grid_connection_point(successors=[mc.make_meter()])
        ),
    ]
    SEEN = {}

    async def run(microgrids):
        SEEN.update(microgrids)
        print("ids", sorted(microgrids), microgrids[2].grpc_url)
        await microgrids[1][load].power.set(Power.from_kilowatts(3))
"""


async def test_the_script_says_hello_then_runs(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    engine = _Engine()
    assert await _run(_script(tmp_path, TWO), engine) == EXIT_FINISHED
    hello = engine.hello
    assert hello["token"] == "tok"
    assert hello["client_version"] == client_version()
    assert hello["topology"]["schema_version"] == 1
    first, second = hello["topology"]["microgrids"]
    # Automatic ids are numbered across the list.
    assert [c["id"] for c in first["components"]] == [1000, 1001]
    assert [c["id"] for c in second["components"]] == [1002, 1003]
    assert "ids [1, 2] grpc://[::1]:8801" in capsys.readouterr().out
    # The script's own spec object found its component.
    assert ("POST", "/api/mg/1/component/1000/drive", {"power_w": 3000.0}) in (
        engine.requests
    )


async def test_a_raising_run_exits_1_with_its_traceback(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    path = _script(
        tmp_path,
        """
        import macrocosim as mc
        MICROGRIDS = [mc.make_microgrid(id=1, topology=mc.make_grid_connection_point())]

        async def run(microgrids):
            raise ValueError("broken")
        """,
    )
    assert await _run(path, _Engine()) == EXIT_RAISED
    err = capsys.readouterr().err
    assert "ValueError: broken" in err
    assert "plant.py" in err and "_runner.py" not in err


@pytest.mark.parametrize(
    ("body", "message"),
    [
        ("x = 1\n", "plant.py defines no MICROGRIDS; it must be a list of"),
        ("MICROGRIDS = ()\n", "plant.py: MICROGRIDS is a tuple; it must be a list"),
        ("MICROGRIDS = [1]\n", "MICROGRIDS[0] is a int; it must be a MicrogridSpec"),
        (
            "import macrocosim as mc\n"
            "MICROGRIDS = [\n"
            "    mc.make_microgrid(topology=mc.make_grid_connection_point())\n"
            "]\n",
            "MICROGRIDS[0] has no id; give it make_microgrid(id=...)",
        ),
        ("MICROGRIDS = []\n", "plant.py defines no run; it must define async def run"),
        (
            "MICROGRIDS = []\ndef run(microgrids):\n    pass\n",
            "plant.py: run is not an async function",
        ),
        (
            "import macrocosim as mc\n"
            "MICROGRIDS = [\n"
            "    mc.make_microgrid(id=1, topology=mc.make_grid_connection_point(id=5)),\n"
            "    mc.make_microgrid(id=2, topology=mc.make_grid_connection_point(id=5)),\n"
            "]\n"
            "async def run(microgrids):\n    pass\n",
            "plant.py: GridConnectionPointSpec(id=5): id 5 is in MicrogridSpec(id=1",
        ),
    ],
)
async def test_a_script_that_does_not_load_exits_2(
    tmp_path: Path, capsys: pytest.CaptureFixture[str], body: str, message: str
) -> None:
    engine = _Engine()
    assert await _run(_script(tmp_path, body), engine) == EXIT_NOT_LOADED
    assert message in capsys.readouterr().err
    assert engine.requests == [], "no hello"


async def test_a_syntax_error_exits_2_naming_the_line(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    path = _script(tmp_path, "x = 1\nMICROGRIDS = [\n")
    assert await _run(path, _Engine()) == EXIT_NOT_LOADED
    err = capsys.readouterr().err
    assert 'plant.py", line 2' in err and "SyntaxError" in err
    assert "runpy" not in err


async def test_a_refused_hello_exits_2_with_the_reason(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    engine = _Engine(409, "microgrid 1 is not this script's: it was made in the UI")
    assert await _run(_script(tmp_path, TWO), engine) == EXIT_NOT_LOADED
    err = capsys.readouterr().err
    assert "the simulator refused the microgrids (409)" in err
    assert "microgrid 1 is not this script's" in err


async def test_the_scripts_directory_is_importable(tmp_path: Path) -> None:
    _script(tmp_path, "LOAD_KW = 4\n", name="runner_test_helper.py")
    path = _script(
        tmp_path,
        """
        import macrocosim as mc
        from runner_test_helper import LOAD_KW
        MICROGRIDS = [mc.make_microgrid(id=1, topology=mc.make_grid_connection_point())]

        async def run(microgrids):
            assert LOAD_KW == 4
        """,
    )
    assert await _run(path, _Engine()) == EXIT_FINISHED


def test_the_output_is_utf8_whatever_the_locale(tmp_path: Path) -> None:
    path = _script(
        tmp_path,
        """
        import time
        print("21 °C", flush=True)
        time.sleep(60)
        """,
    )
    child = subprocess.Popen(
        [sys.executable, "-P", "-m", "macrocosim._runner", str(path)],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        env={
            "MACROCOSIM_URL": URL,
            "MACROCOSIM_RUNNER_TOKEN": "tok",
            "PYTHONIOENCODING": "latin-1",
        },
    )
    assert child.stdout is not None and child.stdin is not None
    try:
        assert child.stdout.readline() == "21 °C\n".encode()
    finally:
        child.stdin.close()
        child.wait(timeout=5)
        child.stdout.close()


def test_the_module_needs_the_engines_environment(tmp_path: Path) -> None:
    out = subprocess.run(
        [sys.executable, "-P", "-m", "macrocosim._runner", str(tmp_path / "x.py")],
        capture_output=True,
        text=True,
        env={"PATH": "/usr/bin:/bin"},
        stdin=subprocess.DEVNULL,
    )
    assert out.returncode == EXIT_NOT_LOADED
    assert "MACROCOSIM_URL and MACROCOSIM_RUNNER_TOKEN are not set" in out.stderr


def test_closing_stdin_ends_the_child(tmp_path: Path) -> None:
    path = _script(
        tmp_path,
        """
        import time
        print("loaded", flush=True)
        time.sleep(60)
        """,
    )
    child = subprocess.Popen(
        [sys.executable, "-P", "-m", "macrocosim._runner", str(path)],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env={"MACROCOSIM_URL": URL, "MACROCOSIM_RUNNER_TOKEN": "tok"},
    )
    assert child.stdout is not None and child.stdin is not None
    assert child.stdout.readline() == b"loaded\n"
    child.stdin.close()
    assert child.wait(timeout=5) == 0
    assert child.stderr is not None
    child.stdout.close()
    child.stderr.close()


@pytest.mark.skipif(os.name != "posix", reason="process groups are POSIX")
@pytest.mark.parametrize(
    ("own_group", "handler"), [(True, "default"), (False, "ignored")]
)
def test_ctrl_c_is_ignored_unless_the_child_leads_its_own_group(
    tmp_path: Path, own_group: bool, handler: str
) -> None:
    path = _script(
        tmp_path,
        """
        import signal
        import time
        h = signal.getsignal(signal.SIGINT)
        print("ignored" if h is signal.SIG_IGN else "default", flush=True)
        time.sleep(60)
        """,
    )
    child = subprocess.Popen(
        [sys.executable, "-P", "-m", "macrocosim._runner", str(path)],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        env={"MACROCOSIM_URL": URL, "MACROCOSIM_RUNNER_TOKEN": "tok"},
        process_group=0 if own_group else None,
    )
    assert child.stdout is not None and child.stdin is not None
    try:
        assert child.stdout.readline().decode().strip() == handler
    finally:
        child.stdin.close()
        child.wait(timeout=5)
        child.stdout.close()
