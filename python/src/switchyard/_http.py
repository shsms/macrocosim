"""Thin synchronous client over switchyard's HTTP ``/api/*`` control plane.

Internal: users reach these calls through :class:`switchyard.runtime.Site`.
Each method maps to one endpoint and returns parsed JSON.
"""

from __future__ import annotations

from typing import Any, TypedDict

import httpx


class EvalResult(TypedDict, total=False):
    """Parsed ``/api/eval`` response. ``ok`` is False (with ``error`` set) when
    the interpreter rejected the form; ``value`` is the evaluated Lisp value."""

    ok: bool
    value: Any
    error: str


class HttpClient:
    """Blocking ``httpx`` client bound to a switchyard UI server."""

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

    def eval(self, expr: str, mg_id: int | None = None) -> EvalResult:
        """POST a Lisp form to ``/api/eval`` (or the per-microgrid variant)."""
        path = "/api/eval" if mg_id is None else f"/api/mg/{mg_id}/eval"
        resp = self._client.post(path, content=expr)
        resp.raise_for_status()
        result: EvalResult = resp.json()
        return result

    def close(self) -> None:
        self._client.close()
