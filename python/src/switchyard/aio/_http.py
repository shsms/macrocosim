"""Async client over switchyard's HTTP ``/api/*`` control plane.

Internal: users reach these calls through :class:`switchyard.aio.Site`.
Each method maps to one endpoint and returns parsed JSON.
"""

from __future__ import annotations

from typing import Any

import httpx

from .._http import EvalResult


class AsyncHttpClient:
    """Async ``httpx`` client bound to a switchyard UI server."""

    def __init__(self, base_url: str, *, timeout: float = 10.0) -> None:
        self._client = httpx.AsyncClient(base_url=base_url, timeout=timeout)

    async def get_json(self, path: str) -> Any:
        """GET ``path`` and return the parsed JSON (shape is endpoint-specific)."""
        resp = await self._client.get(path)
        resp.raise_for_status()
        return resp.json()

    async def post(self, path: str, content: str = "") -> Any:
        """POST a raw body to ``path``; return parsed JSON (or ``{}``)."""
        resp = await self._client.post(path, content=content)
        resp.raise_for_status()
        return resp.json() if resp.content else {}

    async def post_json(self, path: str, payload: Any) -> Any:
        """POST a JSON body to ``path``; return the parsed JSON response.

        Does NOT raise on HTTP error statuses: typed control endpoints
        report rejections as structured JSON, and the caller turns that
        into the right exception.
        """
        resp = await self._client.post(path, json=payload)
        if resp.status_code >= 500:
            resp.raise_for_status()
        return resp.json() if resp.content else {}

    async def eval(self, expr: str, mg_id: int | None = None) -> EvalResult:
        """POST a Lisp form to ``/api/eval`` (or the per-microgrid variant)."""
        path = "/api/eval" if mg_id is None else f"/api/mg/{mg_id}/eval"
        resp = await self._client.post(path, content=expr)
        resp.raise_for_status()
        result: EvalResult = resp.json()
        return result

    async def aclose(self) -> None:
        """Close the underlying connection pool."""
        await self._client.aclose()
