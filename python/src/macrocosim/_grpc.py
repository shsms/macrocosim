"""Sync facade over the async ``frequenz-client-microgrid`` gRPC client.

The upstream client is asyncio-native (``frequenz-channels`` receivers), but
the public surface here is blocking for pytest. We own one event loop on a
background thread, construct the client inside it (grpc aio channels are
loop-bound), and submit each coroutine with
``run_coroutine_threadsafe(...).result()``. This keeps a single live
connection across sync calls — the same client the app under test uses.
"""

from __future__ import annotations

import asyncio
import threading
import time
from collections.abc import Coroutine
from concurrent.futures import Future
from dataclasses import dataclass
from datetime import timedelta
from typing import Any, TypeVar

_T = TypeVar("_T")


@dataclass(frozen=True)
class ComponentInfo:
    """Minimal identity view of a component from ``list_components``."""

    id: int
    category: str
    name: str | None


def _component_id(value: Any) -> Any:
    """Wrap an int id in the client's id type (accepts an id object too).

    The client's ``set_*`` / ``add_*`` paths match on ``ComponentId`` (its
    ``ElectricalComponentId`` successor is rejected in 0.18.3), so use that
    despite the deprecation shim it emits.
    """
    if not isinstance(value, int):
        return value
    import warnings

    with warnings.catch_warnings():
        warnings.simplefilter("ignore", DeprecationWarning)
        from frequenz.client.common.microgrid.components import (
            ComponentId,  # ty: ignore[deprecated]
        )

        return ComponentId(value)  # ty: ignore[deprecated]


class GrpcClient:
    """Blocking wrapper around one ``MicrogridApiClient`` connection."""

    def __init__(self, server_url: str, *, timeout: float = 15.0) -> None:
        self._timeout = timeout
        self._loop = asyncio.new_event_loop()
        self._thread = threading.Thread(
            target=self._loop.run_forever, name="macrocosim-grpc", daemon=True
        )
        self._thread.start()
        self._client = self._run(self._make_client(server_url))
        # One persistent stream per (component, metric); a pump task keeps
        # `_latest` updated so polling reads don't re-open a stream each call.
        # `_primed` marks streams that have delivered at least one sample, so
        # a read of a metric the component never publishes returns fast (None)
        # instead of waiting `first_wait` every time.
        self._latest: dict[tuple[int, str], float | None] = {}
        self._primed: set[tuple[int, str]] = set()
        self._pumps: dict[tuple[int, str], Future[None]] = {}
        # Reads run on asyncio.to_thread worker threads, so two can race
        # to open the same stream; the lock makes pump creation one-shot.
        self._pump_lock = threading.Lock()

    @staticmethod
    async def _make_client(url: str) -> Any:
        from frequenz.client.microgrid import MicrogridApiClient

        # connect=True (default) opens the channel in this loop.
        return MicrogridApiClient(url)

    def _run(self, coro: Coroutine[Any, Any, _T]) -> _T:
        return asyncio.run_coroutine_threadsafe(coro, self._loop).result(self._timeout)

    # --- discovery --------------------------------------------------------

    def components(self) -> list[ComponentInfo]:
        async def go() -> list[ComponentInfo]:
            comps = await self._client.list_components()
            # identity is a (ComponentId, MicrogridId) tuple.
            return [
                ComponentInfo(
                    id=int(c.identity[0]),
                    category=c.category.name,
                    name=getattr(c, "name", None),
                )
                for c in comps
            ]

        return self._run(go())

    # --- reads (latest value off a persistent per-metric stream) ----------

    def active_power(self, component_id: int) -> float | None:
        return self._read_metric(component_id, "AC_POWER_ACTIVE")

    def reactive_power(self, component_id: int) -> float | None:
        return self._read_metric(component_id, "AC_POWER_REACTIVE")

    def soc(self, component_id: int) -> float | None:
        """Battery state of charge (%)."""
        return self._read_metric(component_id, "BATTERY_SOC_PCT")

    def _read_metric(
        self, component_id: int, metric_name: str, *, first_wait: float = 5.0
    ) -> float | None:
        """Latest cached value; opens the stream + waits for the first sample.

        Waits up to ``first_wait`` for the first sample, but stops early if
        the pump has already ended (a stream that errored on subscribe or a
        metric this component never publishes) — so a bad read costs one
        wait, not one per poll, and never busy-waits forever.
        """
        key = (component_id, metric_name)
        with self._pump_lock:
            pump = self._pumps.get(key)
            if pump is None:
                pump = asyncio.run_coroutine_threadsafe(
                    self._pump(component_id, metric_name), self._loop
                )
                self._pumps[key] = pump
        deadline = time.monotonic() + first_wait
        while (
            key not in self._latest
            and key not in self._primed
            and not pump.done()
            and time.monotonic() < deadline
        ):
            time.sleep(0.05)
        return self._latest.get(key)

    async def _pump(self, component_id: int, metric_name: str) -> None:
        """Subscribe once and keep `_latest[(id, metric)]` current.

        Swallows all errors (subscribe failure, stream closed on teardown):
        the task completes, `_read_metric` sees `pump.done()`, and no
        exception is left unretrieved on the future.
        """
        from frequenz.client.microgrid import metrics as m

        metric = m.Metric[metric_name]
        key = (component_id, metric_name)
        try:
            receiver = self._client.receive_component_data_samples_stream(
                _component_id(component_id), [metric]
            )
            while True:
                sample = await receiver.receive()
                for ms in sample.metric_samples:
                    if ms.metric == metric:
                        self._latest[key] = ms.as_single_value()
                # Mark primed only after any value is cached, so a reader that
                # observes `_primed` never races ahead of a same-sample value.
                self._primed.add(key)
        except Exception:  # noqa: BLE001 — end the pump cleanly on any error
            return

    # --- writes -----------------------------------------------------------

    def set_active_power(
        self, component_id: int, watts: float, *, lifetime_s: float | None = None
    ) -> None:
        async def go() -> None:
            lifetime = timedelta(seconds=lifetime_s) if lifetime_s is not None else None
            await self._client.set_component_power_active(
                _component_id(component_id), float(watts), request_lifetime=lifetime
            )

        self._run_command(go())

    def augment_active_power_bounds(
        self, component_id: int, lower: float, upper: float
    ) -> None:
        async def go() -> None:
            from frequenz.client.microgrid import metrics as m

            await self._client.add_component_bounds(
                _component_id(component_id),
                m.Metric.AC_POWER_ACTIVE,
                [m.Bounds(lower=lower, upper=upper)],
            )

        self._run_command(go())

    def _run_command(self, coro: Coroutine[Any, Any, _T]) -> _T:
        """Run a write, re-raising a gateway rejection as SetpointRejected."""
        from frequenz.client.microgrid import ApiClientError

        try:
            return self._run(coro)
        except ApiClientError as exc:
            from .errors import SetpointRejected

            raise SetpointRejected(str(exc)) from exc

    # --- lifecycle --------------------------------------------------------

    def close(self) -> None:
        # Under the same lock _read_metric inserts with — a still-running
        # read must not slip a fresh pump past the drain onto a loop that
        # is about to stop.
        with self._pump_lock:
            for pump in self._pumps.values():
                pump.cancel()
            self._pumps.clear()
        try:
            self._run(self._client.disconnect())
        except Exception:  # noqa: BLE001 — teardown is best-effort
            pass
        self._loop.call_soon_threadsafe(self._loop.stop)
        self._thread.join(timeout=5.0)
