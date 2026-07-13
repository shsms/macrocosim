"""Native-async wrapper over the ``frequenz-client-microgrid`` gRPC client.

Unlike the sync facade (:mod:`switchyard._grpc`), everything here runs on
the *caller's* event loop: no background loop thread, no worker threads,
no locks. One reader task per ``(component, metric)`` stream keeps the
latest sample cached; reads await an :class:`asyncio.Event` instead of
polling.
"""

from __future__ import annotations

import asyncio
from collections.abc import Coroutine
from dataclasses import dataclass, field
from datetime import timedelta
from typing import Any, TypeVar

from .._grpc import ComponentInfo, _component_id
from ..errors import SetpointRejected

_T = TypeVar("_T")


@dataclass
class _Reader:
    """One metric stream's cache: the latest sample and its arrival event."""

    latest: float | None = None
    """The most recent sample value (``None`` until the first arrives)."""

    first: asyncio.Event = field(default_factory=asyncio.Event)
    """Set on the first cached sample — or when the stream ends, so a
    reader of a dead stream returns fast instead of waiting out the
    timeout every call."""

    task: asyncio.Task[None] | None = None
    """The pump task feeding ``latest``."""


class AsyncGrpcClient:
    """Async wrapper around one ``MicrogridApiClient`` connection."""

    def __init__(self, server_url: str, *, timeout: float = 15.0) -> None:
        self._url = server_url
        self._timeout = timeout
        self._client: Any = None
        # One persistent stream per (component, metric); a reader task keeps
        # `_Reader.latest` current so polling reads don't re-open a stream
        # each call. Single-loop by design: no locks needed.
        self._readers: dict[tuple[int, str], _Reader] = {}

    async def _ensure_client(self) -> Any:
        if self._client is None:
            from frequenz.client.microgrid import MicrogridApiClient

            # connect=True (default) opens the channel on this loop.
            self._client = MicrogridApiClient(self._url)
        return self._client

    # --- discovery ---------------------------------------------------------

    async def components(self) -> list[ComponentInfo]:
        """List the microgrid's components (id, category, name)."""
        client = await self._ensure_client()
        comps = await client.list_components()
        # identity is a (ComponentId, MicrogridId) tuple.
        return [
            ComponentInfo(
                id=int(c.identity[0]),
                category=c.category.name,
                name=getattr(c, "name", None),
            )
            for c in comps
        ]

    # --- reads (latest value off a persistent per-metric stream) -----------

    async def active_power(self, component_id: int) -> float | None:
        """A component's active power — the latest cached stream sample."""
        return await self._read_metric(component_id, "AC_POWER_ACTIVE")

    async def soc(self, component_id: int) -> float | None:
        """Battery state of charge (%)."""
        return await self._read_metric(component_id, "BATTERY_SOC_PCT")

    async def _read_metric(
        self,
        component_id: int,
        metric_name: str,
        *,
        first_wait: timedelta = timedelta(seconds=5),
    ) -> float | None:
        """Latest cached value; opens the stream and awaits the first sample.

        Waits up to ``first_wait`` for the first sample, but returns early
        if the reader task ends (a stream that errored on subscribe, or a
        metric this component never publishes) — so a bad read costs one
        wait, not one per poll.
        """
        await self._ensure_client()
        key = (component_id, metric_name)
        reader = self._readers.get(key)
        if reader is None:
            reader = _Reader()
            reader.task = asyncio.create_task(self._pump(key, reader))
            self._readers[key] = reader
        try:
            await asyncio.wait_for(reader.first.wait(), first_wait.total_seconds())
        except TimeoutError:
            pass
        return reader.latest

    async def _pump(self, key: tuple[int, str], reader: _Reader) -> None:
        """Subscribe once and keep ``reader.latest`` current.

        Swallows all errors (subscribe failure, stream closed on teardown):
        the task ends, ``reader.first`` is set, and readers see ``None``.
        """
        from frequenz.client.microgrid import metrics as m

        component_id, metric_name = key
        metric = m.Metric[metric_name]
        try:
            receiver = self._client.receive_component_data_samples_stream(
                _component_id(component_id), [metric]
            )
            while True:
                sample = await receiver.receive()
                for ms in sample.metric_samples:
                    if ms.metric == metric:
                        reader.latest = ms.as_single_value()
                # Signal only after any value is cached, so a waiter never
                # races ahead of a same-sample value.
                reader.first.set()
        except Exception:  # noqa: BLE001 — end the pump cleanly on any error
            return
        finally:
            reader.first.set()

    # --- writes -------------------------------------------------------------

    async def set_active_power(
        self, component_id: int, watts: float, *, lifetime_s: float | None = None
    ) -> None:
        """Command an active-power setpoint through the real gateway."""
        client = await self._ensure_client()
        lifetime = timedelta(seconds=lifetime_s) if lifetime_s is not None else None
        await self._command(
            client.set_component_power_active(
                _component_id(component_id), float(watts), request_lifetime=lifetime
            )
        )

    async def augment_active_power_bounds(
        self, component_id: int, lower: float, upper: float
    ) -> None:
        """Narrow a component's effective active-power bounds."""
        from frequenz.client.microgrid import metrics as m

        client = await self._ensure_client()
        await self._command(
            client.add_component_bounds(
                _component_id(component_id),
                m.Metric.AC_POWER_ACTIVE,
                [m.Bounds(lower=lower, upper=upper)],
            )
        )

    async def _command(self, coro: Coroutine[Any, Any, _T]) -> _T:
        """Run a write, re-raising a gateway rejection as SetpointRejected."""
        from frequenz.client.microgrid import ApiClientError

        try:
            return await coro
        except ApiClientError as exc:
            raise SetpointRejected(str(exc)) from exc

    # --- lifecycle -----------------------------------------------------------

    async def aclose(self) -> None:
        """Cancel the reader tasks and disconnect."""
        readers = list(self._readers.values())
        self._readers.clear()
        for reader in readers:
            if reader.task is not None:
                reader.task.cancel()
        for reader in readers:
            if reader.task is not None:
                try:
                    await reader.task
                except (asyncio.CancelledError, Exception):  # noqa: BLE001
                    pass
        if self._client is not None:
            try:
                await self._client.disconnect()
            except Exception:  # noqa: BLE001 — teardown is best-effort
                pass
            self._client = None
