"""`launch`: start a simulator for a spec, yield its `Microgrid`, stop it.

Internal; imports `_process`, `_render`, `_session`, `_microgrid`, `_spec`,
`_units` and `_errors`.
"""

from __future__ import annotations

import asyncio
import contextlib
import shutil
import time
from collections.abc import AsyncIterator, Callable, Mapping
from datetime import timedelta
from pathlib import Path

import httpx

from . import _process
from ._errors import (
    LaunchError,
    ServerError,
    SimulatorExited,
    fmt_duration,
    fmt_log,
    fmt_tail,
)
from ._microgrid import Microgrid
from ._process import (
    Endpoints,
    _EndpointsTimeout,
    _ExitedEarly,
    _Handle,
    _MalformedEndpoints,
    _NotOneMicrogrid,
    log_tail,
    wait_endpoints,
)
from ._render import render
from ._session import _Session
from ._spec import MicrogridSpec
from ._units import check_type

READY_TIMEOUT = timedelta(seconds=20)
STATUS_POLL_S = 0.25


class _LoopbackTimeout(Exception):
    def __init__(self, graph_status: str) -> None:
        super().__init__(graph_status)
        self.graph_status = graph_status


def _default_binary() -> str:
    return _process.resolve_binary("macrocosim", env_var="MACROCOSIM_BIN")


class _Simulator:
    """The process, its directory and the HTTP session, for teardown."""

    def __init__(self, handle: _Handle) -> None:
        self.handle = handle
        self.session: _Session | None = None

    def exit_probe(self) -> tuple[int, str] | None:
        returncode = self.handle.process.poll()
        if returncode is None:
            return None
        return returncode, log_tail(self.handle)

    async def stop(self) -> str:
        """Stop the process, close the session, then delete a temporary
        directory. Returns the end of the log, read before the delete.

        The process is stopped synchronously, so a cancellation cannot interrupt
        it.
        """
        if self.session is not None:
            self.session.mark_closed()
        tail = ""
        try:
            _process.terminate(self.handle.process)
            tail = log_tail(self.handle)
        finally:
            try:
                if self.session is not None:
                    await self.session.close()
            finally:
                if self.handle.temporary:
                    shutil.rmtree(self.handle.directory, ignore_errors=True)
        return tail


async def _wait_connected(session: _Session, deadline: float) -> None:
    while True:
        status = await session.get_json_object(session.microgrid_path("metrics/status"))
        if status.get("connected"):
            return
        if time.monotonic() >= deadline:
            try:
                topology = await session.get_json_object(
                    session.microgrid_path("topology")
                )
                graph_status = topology.get("graph_status")
                text = "null" if graph_status is None else str(graph_status)
            except SimulatorExited:
                raise
            except ServerError:
                text = "unknown"
            raise _LoopbackTimeout(text)
        await asyncio.sleep(STATUS_POLL_S)


def _as_launch_error(
    exc: BaseException, ready_timeout: timedelta, tail: str, kept_log: Path | None
) -> LaunchError | None:
    """The `LaunchError` for a failure after the spawn: an early exit, no
    endpoints file or a malformed one, not exactly one microgrid in it, no
    loopback connection, or an HTTP failure while checking it. ``None`` for any
    other exception, which then propagates unchanged."""
    after = f"{fmt_tail(tail)}{fmt_log(kept_log)}"
    timeout = fmt_duration(ready_timeout)
    if isinstance(exc, _ExitedEarly):
        text = (
            f"the simulator exited with code {exc.returncode} before writing its "
            "endpoints"
        )
    elif isinstance(exc, _EndpointsTimeout):
        text = f"the simulator wrote no endpoints within {timeout}"
    elif isinstance(exc, _MalformedEndpoints):
        text = (
            f"the simulator's endpoints file is not what the client expects ({exc}); "
            "do the client and the simulator binary match?"
        )
    elif isinstance(exc, _NotOneMicrogrid):
        text = (
            f"the simulator listed {exc.count} microgrids in its endpoints file, "
            "expected 1"
        )
    elif isinstance(exc, _LoopbackTimeout):
        text = (
            f"the simulator's loopback did not connect within {timeout}; "
            f"graph_status: {exc.graph_status}"
        )
    elif isinstance(exc, SimulatorExited):
        text = (
            f"the simulator exited with code {exc.returncode} before its loopback "
            "connected"
        )
    elif isinstance(exc, ServerError):
        text = f"could not check the simulator's loopback: {exc}"
    else:
        return None
    return LaunchError(text + after, log_tail=tail, log_path=kept_log)


@contextlib.asynccontextmanager
async def _launch(
    spec: MicrogridSpec,
    *,
    env: Mapping[str, str] | None = None,
    state_dir: Path | str | None = None,
    ready_timeout: timedelta = READY_TIMEOUT,
    _find_binary: Callable[[], str] = _default_binary,
    _spawn: Callable[..., _Handle] = _process.spawn,
    _transport: httpx.AsyncBaseTransport | None = None,
) -> AsyncIterator[Microgrid]:
    if not isinstance(spec, MicrogridSpec):
        raise TypeError(
            f"launch takes a MicrogridSpec (make_microgrid), not {type(spec).__name__}"
        )
    check_type("ready_timeout", ready_timeout, timedelta)
    directory = None if state_dir is None else Path(state_dir)

    try:
        binary = _find_binary()
    except FileNotFoundError as exc:
        raise LaunchError(str(exc), log_tail="", log_path=None) from exc
    try:
        handle = _spawn(binary, render(spec), env=env, state_dir=directory)
    except OSError as exc:
        raise LaunchError(
            f"could not start the simulator: {exc}", log_tail="", log_path=None
        ) from exc

    kept_log = handle.kept_log
    sim = _Simulator(handle)
    deadline = time.monotonic() + ready_timeout.total_seconds()
    try:
        endpoints: Endpoints = await wait_endpoints(handle, deadline=deadline)
        sim.session = _Session(
            f"http://{endpoints.ui}",
            endpoints.microgrid_id,
            transport=_transport,
            exit_probe=sim.exit_probe,
            log_path=kept_log,
        )
        await _wait_connected(sim.session, deadline)
    except BaseException as exc:
        # Cancellation included: never leave a simulator running.
        tail = await sim.stop()
        error = _as_launch_error(exc, ready_timeout, tail, kept_log)
        if error is not None:
            raise error from exc
        raise

    microgrid = Microgrid._create(sim.session, spec, endpoints)
    try:
        yield microgrid
    finally:
        await sim.stop()


@contextlib.asynccontextmanager
async def launch(
    spec: MicrogridSpec,
    *,
    env: Mapping[str, str] | None = None,
    state_dir: Path | str | None = None,
    ready_timeout: timedelta = READY_TIMEOUT,
) -> AsyncIterator[Microgrid]:
    """Start a simulator running `spec` and yield its live `Microgrid`.

    ::

        async with mc.launch(spec, state_dir=tmp_path / "macrocosim") as microgrid:
            await microgrid[load].power.set(Power.from_kilowatts(20))

    `env` is laid over the test's environment for the simulator process only
    (for example ``FAKETIME``). With `state_dir` (absent or empty), the
    simulator runs there and its log, ``macrocosim.log``, is kept; without it, a
    temporary directory is used and deleted. Leaving the block stops the
    simulator: the client waits up to 5 s for the simulator to exit, then kills
    it.
    """
    async with _launch(
        spec, env=env, state_dir=state_dir, ready_timeout=ready_timeout
    ) as microgrid:
        yield microgrid
