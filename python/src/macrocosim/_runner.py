"""The child process of ``macrocosim plant.py`` (``-m macrocosim._runner``).

Internal; imports `_connect`, `_errors`, `_process`, `_render`, `_session`,
`_spec` and `_microgrid`.

The simulator starts this with the script's path, and with its own address and a
token for this child in ``MACROCOSIM_URL`` and ``MACROCOSIM_RUNNER_TOKEN``. It
runs the script, numbers the component ids of its ``MICROGRIDS``, sends them to
the simulator in a hello, and once the simulator has put them in place, awaits
the script's ``run``.

The simulator holds this process's stdin and never writes to it; when the
simulator closes it, or exits, the process ends. Exit codes: 0 when ``run``
returned, 1 when it raised, 2 when the script did not load or the hello was
refused.
"""

from __future__ import annotations

import asyncio
import inspect
import os
import signal
import sys
import threading
import traceback
import types
from collections.abc import Callable
from pathlib import Path
from typing import Any
from urllib.parse import urlsplit

import httpx

from ._connect import _attach, _no_exit
from ._errors import SpecError
from ._microgrid import Microgrid
from ._process import client_version
from ._render import render
from ._session import _Session, error_text
from ._spec import MicrogridSpec, number_microgrids

EXIT_FINISHED = 0
EXIT_RAISED = 1
EXIT_NOT_LOADED = 2

HELLO_PATH = "/api/runner/hello"


class _NotLoaded(Exception):
    """The script did not load or the hello was refused; the message, if any, is
    printed before the child exits."""


def _say(message: str) -> None:
    print(f"macrocosim: {message}", file=sys.stderr, flush=True)


def _start_watchdog(fd: int) -> None:
    """End the process as soon as the file descriptor `fd` reaches end-of-file.

    It reads the descriptor itself, not `sys.stdin`, whose lock a thread blocked
    in a read would hold while the interpreter shuts down.
    """

    def watch() -> None:
        try:
            while os.read(fd, 4096):
                pass
        except OSError:
            pass
        os._exit(EXIT_FINISHED)

    threading.Thread(target=watch, name="macrocosim-stdin", daemon=True).start()


def _print_error(exc: BaseException) -> None:
    """Print `exc`'s traceback without the frames of this module, which only say
    how the script was run."""
    tb = exc.__traceback__
    while tb is not None and tb.tb_frame.f_code.co_filename == __file__:
        tb = tb.tb_next
    traceback.print_exception(type(exc), exc, tb)


def _load(script: Path) -> tuple[list[MicrogridSpec], Callable[..., Any]]:
    """Run `script` as ``__main__``, as ``python plant.py`` would, and read its
    ``MICROGRIDS`` and ``run``.

    The script's module stays ``sys.modules["__main__"]`` while ``run`` runs, so
    its classes pickle and its type hints resolve, and ``sys.argv`` is
    ``[script]``.
    """
    sys.path.insert(0, str(script.parent))
    sys.argv = [str(script)]
    module = types.ModuleType("__main__")
    module.__file__ = str(script)
    sys.modules["__main__"] = module
    try:
        code = compile(script.read_bytes(), str(script), "exec")
        exec(code, module.__dict__)
    except BaseException as exc:
        _print_error(exc)
        raise _NotLoaded() from None
    namespace = module.__dict__
    name = script.name
    expected = (
        "a list of MicrogridSpec (make_microgrid), each with an id: "
        "MICROGRIDS = [make_microgrid(id=..., topology=...)]"
    )
    if "MICROGRIDS" not in namespace:
        raise _NotLoaded(f"{name} defines no MICROGRIDS; it must be {expected}")
    specs = namespace["MICROGRIDS"]
    if not isinstance(specs, list):
        raise _NotLoaded(
            f"{name}: MICROGRIDS is a {type(specs).__name__}; it must be {expected}"
        )
    for i, spec in enumerate(specs):
        if not isinstance(spec, MicrogridSpec):
            raise _NotLoaded(
                f"{name}: MICROGRIDS[{i}] is a {type(spec).__name__}; it must be "
                "a MicrogridSpec (make_microgrid)"
            )
        if spec.id is None:
            raise _NotLoaded(
                f"{name}: MICROGRIDS[{i}] has no id; give it make_microgrid(id=...), "
                "so that each save replaces it instead of adding another"
            )
    run = namespace.get("run")
    if run is None:
        raise _NotLoaded(
            f"{name} defines no run; it must define "
            "async def run(microgrids: dict[int, Microgrid])"
        )
    if not inspect.iscoroutinefunction(run):
        raise _NotLoaded(
            f"{name}: run is not an async function; it must be "
            "async def run(microgrids: dict[int, Microgrid])"
        )
    return specs, run


async def _hello(
    url: str,
    token: str,
    specs: list[MicrogridSpec],
    transport: httpx.AsyncBaseTransport | None,
) -> dict[int, str | None]:
    """Send the hello and wait, with no timeout, until the simulator has put the
    microgrids in place; the gRPC address of each."""
    body = {
        "token": token,
        "client_version": client_version(),
        "topology": render(*specs),
    }
    async with httpx.AsyncClient(
        base_url=url, transport=transport, trust_env=False, timeout=None
    ) as client:
        try:
            response = await client.post(HELLO_PATH, json=body)
        except httpx.TransportError as exc:
            raise _NotLoaded(f"could not send the hello to {url}: {exc}") from exc
    if response.status_code != 200:
        raise _NotLoaded(
            f"the simulator refused the microgrids ({response.status_code}): "
            f"{error_text(response)}"
        )
    try:
        return {
            int(m["id"]): None if m["grpc_addr"] is None else str(m["grpc_addr"])
            for m in response.json()["microgrids"]
        }
    except (KeyError, TypeError, ValueError) as exc:
        raise _NotLoaded(
            f"the simulator answered the hello with something this client does not "
            f"read ({exc!r}); do the client and the simulator binary match?"
        ) from exc


async def run_script(
    script: Path,
    url: str,
    token: str,
    *,
    transport: httpx.AsyncBaseTransport | None = None,
) -> int:
    """Load `script`, send its microgrids to the simulator at `url` and await
    its ``run``; the exit code."""
    try:
        specs, run = _load(script)
        try:
            number_microgrids(specs)
        except SpecError as exc:
            raise _NotLoaded(f"{script.name}: {exc}") from exc
        addresses = await _hello(url, token, specs, transport)
    except _NotLoaded as exc:
        if exc.args:
            _say(str(exc.args[0]))
        return EXIT_NOT_LOADED
    ui = urlsplit(url).netloc
    sessions: list[_Session] = []
    microgrids: dict[int, Microgrid] = {}
    for spec in specs:
        assert spec.id is not None  # _load checked
        mg_id = int(spec.id)
        session = _Session(url, mg_id, transport=transport, exit_probe=_no_exit)
        sessions.append(session)
        microgrids[mg_id] = _attach(session, spec, ui, addresses.get(mg_id))
    try:
        await run(microgrids)
    except Exception as exc:
        _print_error(exc)
        return EXIT_RAISED
    finally:
        for session in sessions:
            await session.close()
    return EXIT_FINISHED


def _leads_own_group() -> bool:
    """Whether this process leads a process group of its own."""
    return os.name == "posix" and os.getpgrp() == os.getpid()


def main(argv: list[str] | None = None) -> int:
    """The child's entry point: ``python -m macrocosim._runner SCRIPT``."""
    args = sys.argv[1:] if argv is None else argv
    # The simulator stops this process by closing its stdin, not with the
    # terminal's Ctrl-C. On POSIX it runs this process as the leader of a
    # process group of its own, which the terminal's Ctrl-C does not reach;
    # otherwise the Ctrl-C is ignored here.
    if not _leads_own_group():
        signal.signal(signal.SIGINT, signal.SIG_IGN)
    # The simulator reads the output as UTF-8, line by line.
    for stream in (sys.stdout, sys.stderr):
        reconfigure = getattr(stream, "reconfigure", None)
        if reconfigure is not None:
            reconfigure(encoding="utf-8", errors="backslashreplace", line_buffering=True)
    if len(args) != 1:
        _say("usage: python -m macrocosim._runner SCRIPT")
        return EXIT_NOT_LOADED
    url = os.environ.get("MACROCOSIM_URL")
    token = os.environ.get("MACROCOSIM_RUNNER_TOKEN")
    if not url or not token:
        _say(
            "MACROCOSIM_URL and MACROCOSIM_RUNNER_TOKEN are not set; the simulator "
            "starts this module itself (macrocosim plant.py)"
        )
        return EXIT_NOT_LOADED
    # After the checks above, so a misuse is reported even when stdin is already
    # closed.
    _start_watchdog(sys.stdin.fileno())
    return asyncio.run(run_script(Path(args[0]).resolve(), url, token))


if __name__ == "__main__":
    sys.exit(main())
