"""`connect`: attach to the microgrids of a simulator that is already running.

Internal; imports `_errors`, `_process`, `_session`, `_spec`, `_topology`,
`_microgrid` and `_units`.
"""

from __future__ import annotations

import contextlib
from collections.abc import AsyncIterator
from typing import Any
from urllib.parse import urlsplit

import httpx

from ._errors import ServerError
from ._microgrid import Microgrid
from ._process import Endpoints
from ._session import _Session
from ._spec import MicrogridSpec
from ._topology import microgrid_from_value
from ._units import check_type


def _no_exit() -> None:
    """The exit probe of a simulator this client did not start: it never knows
    that the simulator exited."""
    return None


def _attach(
    session: _Session, spec: MicrogridSpec, ui: str, grpc_addr: str | None
) -> Microgrid:
    """The live `Microgrid` of `spec` over `session`, at the simulator whose
    UI serves at `ui` (``host:port``) and whose gRPC server for it is at
    `grpc_addr` (``None`` when it has none)."""
    endpoints = Endpoints(
        ui=ui, microgrid_id=session.microgrid_id, grpc_addr=grpc_addr or ""
    )
    return Microgrid._create(session, spec, endpoints)


def _listing(body: Any) -> list[tuple[int, str | None]]:
    """The id of each listed microgrid, and the address of its gRPC server when
    that server runs."""
    found = []
    for entry in body:
        runtime = entry["runtime"]
        running = runtime is not None and runtime["status"] == "running"
        addr = runtime["grpc_addr"] if running else None
        found.append((int(entry["id"]), None if addr is None else str(addr)))
    return found


@contextlib.asynccontextmanager
async def _connect(
    url: str, *, _transport: httpx.AsyncBaseTransport | None = None
) -> AsyncIterator[dict[int, Microgrid]]:
    check_type("url", url, str)
    parts = urlsplit(url)
    if parts.scheme != "http" or not parts.netloc:
        raise ValueError(
            f"connect takes the simulator's UI address, such as "
            f"'http://127.0.0.1:8801', not {url!r}"
        )
    base = f"http://{parts.netloc}"
    sessions: list[_Session] = []

    def session(microgrid_id: int) -> _Session:
        s = _Session(base, microgrid_id, transport=_transport, exit_probe=_no_exit)
        sessions.append(s)
        return s

    try:
        listed = await session(0).get_parsed("/api/microgrids", _listing)
        microgrids: dict[int, Microgrid] = {}
        for microgrid_id, grpc_addr in listed:
            s = session(microgrid_id)
            try:
                body = await s.get_json(s.microgrid_path("spec"))
            except ServerError as exc:
                if exc.status != 404:
                    raise
                relisted = await session(0).get_parsed("/api/microgrids", _listing)
                if any(other == microgrid_id for other, _ in relisted):
                    raise
                # Unloaded since it was listed.
                continue
            spec = microgrid_from_value(body)
            microgrids[microgrid_id] = _attach(s, spec, parts.netloc, grpc_addr)
        yield microgrids
    finally:
        for s in sessions:
            await s.close()


@contextlib.asynccontextmanager
async def connect(url: str) -> AsyncIterator[dict[int, Microgrid]]:
    """Attach to the simulator whose UI serves at `url`, and yield its
    microgrids by id.

    ::

        async with mc.connect("http://127.0.0.1:8801") as microgrids:
            await microgrids[7][1003].power.set(Power.from_kilowatts(20))

    Each `Microgrid` is built from the spec the simulator stores for it, so its
    components are found by id (``microgrid[1003]``). Leaving the block closes
    the connections; the simulator keeps running.
    """
    async with _connect(url) as microgrids:
        yield microgrids
