"""Thin synchronous client over macrocosim's HTTP ``/api/*`` control plane.

Internal: users reach these calls through :class:`macrocosim.runtime.Site`.
Each method maps to one endpoint and returns parsed JSON.
"""

from __future__ import annotations

from typing import Any, TypedDict

import httpx

from .errors import ControlRejected


class EvalResult(TypedDict, total=False):
    """Parsed ``/api/eval`` response. ``ok`` is False (with ``error`` set) when
    the interpreter rejected the form; ``value`` is the evaluated Lisp value."""

    ok: bool
    value: Any
    error: str


def control_path(component_id: int, action: str, mg_id: int | None) -> str:
    """The control endpoint for one component action (``status``/``drive``).

    One place builds the route for both client flavors, so a route change
    on the server cannot be missed in one of them.
    """
    if mg_id is None:
        return f"/api/component/{component_id}/{action}"
    return f"/api/mg/{mg_id}/component/{component_id}/{action}"


class HttpClient:
    """Blocking ``httpx`` client bound to a macrocosim UI server."""

    def __init__(self, base_url: str, *, timeout: float = 10.0) -> None:
        self._client = httpx.Client(base_url=base_url, timeout=timeout)

    def get_json(self, path: str) -> Any:
        """GET ``path`` and return the parsed JSON (shape is endpoint-specific)."""
        resp = self._client.get(path)
        resp.raise_for_status()
        return resp.json()

    def post(self, path: str, content: str = "") -> Any:
        resp = self._client.post(path, content=content)
        resp.raise_for_status()
        return resp.json() if resp.content else {}

    def control(self, path: str, payload: dict[str, Any]) -> Any:
        """POST a typed control request; a 4xx rejection raises.

        The control endpoints report rejections as structured JSON
        (``{"error": ...}``) with a 400/404 status — turn that into
        :class:`ControlRejected` so a rejection can never silently no-op.
        """
        resp = self._client.post(path, json=payload)
        if 400 <= resp.status_code < 500:
            try:
                error = resp.json().get("error", resp.text)
            except ValueError:
                error = resp.text
            raise ControlRejected(error)
        resp.raise_for_status()
        return resp.json() if resp.content else {}

    def eval(self, expr: str, mg_id: int | None = None) -> EvalResult:
        """POST a Lisp form to ``/api/eval`` (or the per-microgrid variant)."""
        path = "/api/eval" if mg_id is None else f"/api/mg/{mg_id}/eval"
        resp = self._client.post(path, content=expr)
        resp.raise_for_status()
        result: EvalResult = resp.json()
        return result

    def close(self) -> None:
        self._client.close()
