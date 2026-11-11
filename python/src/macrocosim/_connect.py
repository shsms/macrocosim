"""`connect`: attach to a simulator that is already running, and declare
microgrids on it.

Internal; imports `_errors`, `_render`, `_process`, `_session`, `_spec`,
`_topology`, `_microgrid` and `_units`.
"""

from __future__ import annotations

import contextlib
import dataclasses
import logging
import os
from collections.abc import AsyncIterator, Callable
from typing import Any, overload
from urllib.parse import urlsplit

import httpx

from ._errors import ConnectionClosed, MacrocosimError, MicrogridClosed, ServerError
from ._microgrid import Microgrid
from ._process import Endpoints, client_version
from ._render import render
from ._session import CLOSED, _Session
from ._spec import FIRST_AUTO_ID, MicrogridSpec, _assign_ids, _check_explicit_ids
from ._topology import microgrid_from_value
from ._units import check_type

_log = logging.getLogger("macrocosim")

DEFAULT_URL = "http://127.0.0.1:8801"
"""The UI address `connect` uses when neither its argument nor
``$MACROCOSIM_URL`` names one: the server's default UI port."""


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


def _connection_closed_error(what: str) -> ConnectionClosed:
    """The error of a call on a closed connection: `what` happened to it."""
    return ConnectionClosed(f"the connection {what}")


def _running_addr(runtime: Any) -> str | None:
    """The gRPC address of a listed or declared microgrid's `runtime` while its
    server runs."""
    running = runtime is not None and runtime["status"] == "running"
    return str(runtime["grpc_addr"]) if running else None


def _listing(body: Any) -> list[tuple[int, str | None]]:
    """The id of each listed microgrid, and the address of its gRPC server when
    that server runs."""
    return [(int(entry["id"]), _running_addr(entry["runtime"])) for entry in body]


def _declared(body: Any) -> list[tuple[int, str, Any]]:
    """The id, outcome and runtime of each microgrid of a declaration's answer.
    An entry without an `outcome` (from a server that does not know the
    ``existing=rebuild`` that `declare` sends) makes the answer unreadable,
    which `declare` raises as `ServerError`."""
    return [
        (int(entry["id"]), str(entry["outcome"]), entry["runtime"])
        for entry in body["microgrids"]
    ]


class Connection:
    """A connection to a simulator that is already running, as `connect` yields
    it.

    Declare microgrids with `declare`, unload one with `unload`, and attach to
    whatever is loaded with `microgrids`. Leaving the `connect` block closes the
    connection: a call that needs the simulator then raises `ConnectionClosed`,
    and the microgrids stay loaded.
    """

    def __init__(
        self,
        base: str,
        ui: str,
        transport: httpx.AsyncBaseTransport | None,
    ) -> None:
        self._base = base
        self._ui = ui
        self._transport = transport
        self._sessions: list[_Session] = []
        self._by_id: dict[int, _Session] = {}
        self._next_auto = FIRST_AUTO_ID
        self._auto_starts: dict[int, int] = {}
        self._numbered_ids: dict[int, set[int]] = {}
        self._closed = False
        self._root = self._open_session(0, closed_error=_connection_closed_error)

    def _open_session(
        self,
        microgrid_id: int,
        closed_error: Callable[[str], MacrocosimError] | None = None,
    ) -> _Session:
        session = _Session(
            self._base,
            microgrid_id,
            transport=self._transport,
            exit_probe=_no_exit,
            closed_error=closed_error,
        )
        self._sessions.append(session)
        return session

    def _session_for(self, microgrid_id: int) -> _Session:
        """The session of microgrid `microgrid_id`, opened on first use. Once
        the connection is closed, raises `ConnectionClosed` instead."""
        if self._closed:
            raise _connection_closed_error(CLOSED)
        session = self._by_id.get(microgrid_id)
        if session is None:
            session = self._by_id[microgrid_id] = self._open_session(microgrid_id)
        return session

    async def _close(self) -> None:
        self._closed = True
        for session in self._sessions:
            await session.close()

    @overload
    async def declare(self, spec: MicrogridSpec, /) -> Microgrid: ...

    @overload
    async def declare(
        self, spec: MicrogridSpec, other: MicrogridSpec, /, *more: MicrogridSpec
    ) -> tuple[Microgrid, ...]: ...

    async def declare(self, *specs: MicrogridSpec) -> Microgrid | tuple[Microgrid, ...]:
        """Create each of `specs` whose microgrid id is not loaded, rebuild each
        loaded one whose spec changed, keeping the state of every component
        whose id and spec did not change, and leave alone each one whose spec is
        the stored one once both have their defaults filled in. Return their
        live `Microgrid`s: one for one spec, a tuple in the same order for
        several.

        ::

            microgrid = await sim.declare(MICROGRID)
            await microgrid[SOLAR].sunlight.set(Percentage.from_percent(80))

        The specs go in one request. Every spec needs a microgrid id. A
        component spec without an id gets one. The first time this connection
        declares a microgrid, its ids start where the connection's earlier
        declarations ended (at 1000 for the first one). When it declares the
        same microgrid again, they start at the same place. The ids this
        connection gave its other microgrids, automatic or explicit, and the
        explicit ids of the call are skipped; an explicit id in a later call is
        not. `specs` themselves are not changed, so the same spec under `launch`
        keeps its own numbering.

        The simulator refusing the declaration (a component id or a gRPC port
        held by a microgrid the call does not list, another package version)
        raises `Rejected`; an invalid spec raises `ServerError`. A microgrid
        whose gRPC server could not start raises `ServerError` once every
        microgrid is applied; declaring it again retries the start.
        """
        if not specs:
            raise TypeError("declare takes at least one MicrogridSpec")
        for spec in specs:
            check_type("specs item", spec, MicrogridSpec)
            if spec.id is None:
                raise ValueError(
                    f"{spec!r} has no microgrid id; declare needs one, so that a "
                    "re-run updates the same microgrid"
                )
        explicit = _check_explicit_ids(specs)
        numbered = [self._numbered_copy(spec, explicit) for spec in specs]
        answer = await self._root.post_parsed(
            "/api/microgrids",
            render(*numbered),
            _declared,
            call="declare",
            params={"existing": "rebuild", "client_version": client_version()},
        )
        microgrids: list[Microgrid] = []
        failed: list[str] = []
        for spec, (microgrid_id, _, runtime) in zip(numbered, answer, strict=True):
            session = self._session_for(microgrid_id)
            microgrids.append(_attach(session, spec, self._ui, _running_addr(runtime)))
            if runtime is not None and runtime["status"] == "failed":
                failed.append(f"microgrid {microgrid_id}: {runtime.get('error')}")
        if failed:
            raise ServerError(
                "the gRPC server did not start for "
                + "; ".join(failed)
                + "; declare it again to retry",
                method="POST",
                path="/api/microgrids",
            )
        return microgrids[0] if len(microgrids) == 1 else tuple(microgrids)

    def _numbered_copy(
        self, spec: MicrogridSpec, explicit: frozenset[int]
    ) -> MicrogridSpec:
        """A copy of `spec` with its automatic component ids numbered, skipping
        `explicit`, the explicit ids of the call (see `declare`). Records where
        the numbering started and every id the copy holds, so a later call
        numbers the same microgrid from the same start and skips these ids for
        the others. The copy shares `spec`'s component specs, so the returned
        `Microgrid` finds a component by the caller's spec object."""
        assert spec.id is not None, "declare checked it"
        microgrid_id = int(spec.id)
        start = self._auto_starts.get(microgrid_id, self._next_auto)
        skip = set(explicit)
        for other, numbered in self._numbered_ids.items():
            if other != microgrid_id:
                skip |= numbered
        copy = dataclasses.replace(spec)
        order, ids, next_id = _assign_ids(copy.topology, start, frozenset(skip))
        object.__setattr__(copy, "_order", order)
        object.__setattr__(copy, "_ids", ids)
        self._auto_starts[microgrid_id] = start
        self._numbered_ids[microgrid_id] = set(ids.values())
        self._next_auto = max(self._next_auto, next_id)
        return copy

    async def unload(self, microgrid_id: int) -> None:
        """Unload microgrid `microgrid_id`: its gRPC server stops, and its id
        and component ids are free again. One that is not loaded raises
        `ServerError`."""
        check_type("microgrid_id", microgrid_id, int)
        await self._root.delete(f"/api/mg/{microgrid_id}", call="unload")

    async def microgrids(self) -> dict[int, Microgrid]:
        """Every microgrid loaded now, by id, built from the spec the simulator
        stores for it: find its components by id (``microgrid[1003]``)."""
        listed = await self._root.get_parsed("/api/microgrids", _listing)
        found: dict[int, Microgrid] = {}
        for microgrid_id, grpc_addr in listed:
            session = self._session_for(microgrid_id)
            path = session.microgrid_path("spec")
            try:
                body = await session.get_json(path)
            except MicrogridClosed as exc:
                # The connect block exited during the request.
                raise _connection_closed_error(f"was closed during GET {path}") from exc
            except ServerError as exc:
                if exc.status != 404:
                    raise
                relisted = await self._root.get_parsed("/api/microgrids", _listing)
                if any(other == microgrid_id for other, _ in relisted):
                    raise
                # Unloaded since it was listed.
                continue
            spec = microgrid_from_value(body)
            found[microgrid_id] = _attach(session, spec, self._ui, grpc_addr)
        return found


def _resolve_url(url: str | None) -> str:
    if url is None:
        url = os.environ.get("MACROCOSIM_URL") or DEFAULT_URL
    check_type("url", url, str)
    return url


@contextlib.asynccontextmanager
async def _connect(
    url: str | None, *, _transport: httpx.AsyncBaseTransport | None = None
) -> AsyncIterator[Connection]:
    url = _resolve_url(url)
    parts = urlsplit(url)
    if parts.scheme != "http" or not parts.netloc:
        raise ValueError(
            f"connect takes the simulator's UI address, such as {DEFAULT_URL!r}, "
            f"not {url!r}"
        )
    _log.info("connecting to the simulator at %s", url)
    connection = Connection(f"http://{parts.netloc}", parts.netloc, _transport)
    try:
        await connection._root.get_parsed("/api/microgrids", _listing)
        yield connection
    finally:
        await connection._close()


@contextlib.asynccontextmanager
async def connect(url: str | None = None) -> AsyncIterator[Connection]:
    """Attach to the simulator whose UI serves at `url`, and yield a
    `Connection` to it.

    ::

        async with mc.connect() as sim:
            microgrid = await sim.declare(MICROGRID)
            await microgrid[SOLAR].sunlight.set(Percentage.from_percent(80))

    Without `url`, it is ``$MACROCOSIM_URL``, else ``http://127.0.0.1:8801``.
    Entering the block checks that the simulator answers. Leaving it closes the
    connection; the simulator keeps running, and so do the microgrids declared
    through it.
    """
    async with _connect(url) as connection:
        yield connection
