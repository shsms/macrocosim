"""The HTTP session to one launched simulator, and how its answers become
errors.

Internal; imports `_errors`. Every request goes through `_Session`, which
refuses to run once the microgrid is closed, and maps each failure to the error
tree: a refused value or fault (400/409 from ``/drive`` or ``/status``) is
`Rejected`, any other non-2xx answer or transport failure is `ServerError`, or
`SimulatorExited` when the process is gone.
"""

from __future__ import annotations

from collections.abc import Callable, Mapping
from pathlib import Path
from typing import Any, TypeVar

import httpx

from ._errors import (
    WRONG_SHAPE,
    MicrogridClosed,
    Rejected,
    ServerError,
    SimulatorExited,
    fmt_log,
    fmt_tail,
)

ExitProbe = Callable[[], "tuple[int, str] | None"]
"""``None`` while the simulator runs; else its return code and log tail."""


T = TypeVar("T")


def _require_json_object(body: Any) -> dict[str, Any]:
    if not isinstance(body, dict):
        raise TypeError(f"not a JSON object: {body!r:.80}")
    return body


def error_text(response: httpx.Response) -> str:
    """The ``error`` field of a failed response, or its body."""
    try:
        body = response.json()
    except ValueError:
        return response.text
    if isinstance(body, dict) and "error" in body:
        return str(body["error"])
    return response.text


class _Session:
    """One `httpx.AsyncClient` bound to a simulator's UI server."""

    def __init__(
        self,
        base_url: str,
        microgrid_id: int,
        *,
        transport: httpx.AsyncBaseTransport | None = None,
        exit_probe: ExitProbe,
        log_path: Path | None = None,
    ) -> None:
        self.microgrid_id = microgrid_id
        self.log_path = log_path
        """The kept simulator log, named in messages; ``None`` without a
        ``state_dir``."""
        self._exit_probe = exit_probe
        self._closed = False
        # trust_env=False: proxy variables in the test's environment must not
        # route loopback traffic. No Origin header: the server refuses
        # browser-origin writes.
        self._client = httpx.AsyncClient(
            base_url=base_url, transport=transport, trust_env=False, timeout=10.0
        )

    def microgrid_path(self, rel: str) -> str:
        """The path of `rel` under this microgrid: ``/api/mg/{id}/{rel}``."""
        return f"/api/mg/{self.microgrid_id}/{rel}"

    def mark_closed(self) -> None:
        """From now on, every call raises `MicrogridClosed` without any I/O, and
        so does a request already running that loses its connection."""
        self._closed = True

    async def close(self) -> None:
        """Refuse every later call and close the HTTP client."""
        self._closed = True
        await self._client.aclose()

    async def get_json(self, path: str, params: Mapping[str, Any] | None = None) -> Any:
        """GET `path`; the decoded JSON body."""
        return await self._request("GET", path, params=params)

    async def get_parsed(
        self,
        path: str,
        parse: Callable[[Any], T],
        params: Mapping[str, Any] | None = None,
    ) -> T:
        """GET `path` and return `parse` of the decoded body; an answer that
        `parse` cannot read raises `ServerError`."""
        body = await self.get_json(path, params)
        try:
            return parse(body)
        except WRONG_SHAPE as exc:
            raise ServerError(
                f"GET {path} answered something the client did not expect "
                f"({exc!r}); do the client and the simulator binary match?",
                method="GET",
                path=path,
            ) from exc

    async def get_json_object(self, path: str) -> dict[str, Any]:
        """GET `path`; the decoded body. An answer that is not a JSON object
        raises `ServerError`."""
        return await self.get_parsed(path, _require_json_object)

    async def post_json(self, path: str, body: Mapping[str, Any], *, call: str) -> Any:
        """POST `body` to `path`; the decoded body, or ``None`` for 204.

        `call` describes the user's call (``"Meter 104 power.set(30 kW)"``). A
        400 or 409 answer means the simulator refused it, and raises `Rejected`
        with that description.
        """
        return await self._request("POST", path, json=body, call=call)

    async def _request(
        self,
        method: str,
        path: str,
        *,
        params: Mapping[str, Any] | None = None,
        json: Mapping[str, Any] | None = None,
        call: str | None = None,
    ) -> Any:
        if self._closed:
            raise self._closed_error("is closed: its launch block has exited")
        try:
            response = await self._client.request(method, path, params=params, json=json)
        except httpx.TransportError as exc:
            if self._closed:
                # The launch block stopped the simulator under this request.
                raise self._closed_error(f"was closed during {method} {path}") from exc
            raise self._transport_error(method, path, exc) from exc
        status = response.status_code
        if 200 <= status < 300:
            if status == 204 or not response.content:
                return None
            try:
                return response.json()
            except ValueError as exc:
                raise ServerError(
                    f"{method} {path} answered {status} with a body that is not JSON",
                    method=method,
                    path=path,
                    status=status,
                ) from exc
        text = error_text(response)
        if call is not None and status in (400, 409):
            raise Rejected(f"{call} was rejected: {text}", call=call, text=text)
        raise ServerError(
            f"{method} {path} answered {status}: {text}",
            method=method,
            path=path,
            status=status,
        )

    def _closed_error(self, what: str) -> MicrogridClosed:
        return MicrogridClosed(
            f"microgrid {self.microgrid_id} {what}", microgrid_id=self.microgrid_id
        )

    def _transport_error(
        self, method: str, path: str, exc: httpx.TransportError
    ) -> ServerError:
        exited = self._exit_probe()
        if exited is None:
            reason = str(exc) or type(exc).__name__
            return ServerError(
                f"{method} {path} failed: {reason}", method=method, path=path
            )
        returncode, tail = exited
        return SimulatorExited(
            f"the simulator exited with code {returncode} during {method} {path}"
            f"{fmt_tail(tail)}{fmt_log(self.log_path)}",
            method=method,
            path=path,
            returncode=returncode,
            log_tail=tail,
            log_path=self.log_path,
        )
