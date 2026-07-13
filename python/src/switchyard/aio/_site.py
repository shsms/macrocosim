"""The async-native ``Site``: every read, write, and wait is a coroutine.

Everything runs on the caller's event loop — no background loop thread,
no worker threads, no locks. This is the v2 core
(``docs/python-api-redesign.org``); the sync client remains available for
REPL use and simple scripts.

The assertion surface here is *generic-first*: pass a metric spec from
:mod:`switchyard.metrics` instead of calling a named method::

    async with sw.aio.launch(topology) as site:
        await site[5].drive(power=Power.from_kilowatts(20))
        await site.expect(GRID_POWER, max=Power.from_kilowatts(13))
        await site.expect(BATTERY_ENERGY, max=Energy.from_watt_hours(-1))

The metric's *kind* picks the check semantics (settle vs one-shot), and a
new metric is a catalog entry, not a set of new methods.
"""

from __future__ import annotations

import asyncio
import functools
import json
import subprocess
import time
from collections.abc import AsyncIterator, Awaitable, Callable
from contextlib import asynccontextmanager
from datetime import timedelta
from typing import TYPE_CHECKING, Any, TypeVar, cast

from frequenz.quantities import Energy, Percentage, Power, Quantity

from .. import metrics as _M
from .._http import EvalResult
from .._process import spawn_switchyard, terminate
from ..assertions import expect_metric
from ..build import RawLisp, to_lisp_atom
from ..errors import EvalRejected
from ..metrics import BoundMetric, MetricSpec
from ..runtime import MicrogridEndpoint
from ._grpc import AsyncGrpcClient
from ._http import AsyncHttpClient

if TYPE_CHECKING:
    import os

    from .._grpc import ComponentInfo
    from ..build import Component, LaunchConfig
    from ..enums import CommandMode, Health, TelemetryMode
    from ..scenarios import JournalEvent, Scenario, ScenarioReport

Q = TypeVar("Q", bound=Quantity)
T = TypeVar("T")

# Aggregate metric name -> the formula stream serving it. Only the battery
# pool's stream name differs from its metric name.
_STREAM_FOR = {
    "grid_power": "grid_power",
    "pv_power": "pv_power",
    "consumer_power": "consumer_power",
    "battery_power": "battery_pool_power",
    "grid_energy": "grid_energy",
    "pv_energy": "pv_energy",
    "consumer_energy": "consumer_energy",
    "battery_energy": "battery_pool_energy",
}

# Quantity type -> constructor from the wire float (W / Wh / %).
_FROM_WIRE: dict[type[Quantity], Callable[[float], Quantity]] = {
    Power: Power.from_watts,
    Energy: Energy.from_watt_hours,
    Percentage: Percentage.from_percent,
}


class Site:
    """Async handle onto a running switchyard.

    Mirrors the sync ``switchyard.Site`` surface, with every read, write,
    and wait a coroutine on the caller's loop.
    """

    def __init__(
        self,
        *,
        ui: str,
        microgrids: dict[int, MicrogridEndpoint],
        assets: str | None = None,
        dispatch: str | None = None,
        process: subprocess.Popen[bytes] | None = None,
    ) -> None:
        self.ui = ui
        self.microgrids = microgrids
        self.assets = assets
        self.dispatch = dispatch
        self._process = process
        self._http = AsyncHttpClient(f"http://{ui}")
        # Single-loop by design: creation is not racy, no lock needed.
        self._grpc_clients: dict[int, AsyncGrpcClient] = {}

    # --- endpoints -----------------------------------------------------------

    @property
    def grpc(self) -> str:
        """``host:port`` of the first (default) microgrid's gRPC API."""
        return self.microgrids[self._resolve_mg(None)].grpc

    @property
    def grpc_url(self) -> str:
        """The default microgrid's gRPC API as a ``grpc://host:port`` URL."""
        return f"grpc://{self.grpc}"

    def microgrid_grpc_url(self, mg_id: int) -> str:
        """A specific microgrid's gRPC API as a ``grpc://host:port`` URL."""
        return f"grpc://{self.microgrids[mg_id].grpc}"

    def _resolve_mg(self, mg_id: int | None) -> int:
        if mg_id is not None:
            return mg_id
        if not self.microgrids:
            raise RuntimeError(
                "this Site has no microgrid endpoints; launch() discovers them, "
                "connect() needs microgrids={id: MicrogridEndpoint(...)}"
            )
        return next(iter(self.microgrids))

    def _grpc(self, mg_id: int | None = None) -> AsyncGrpcClient:
        mg = self._resolve_mg(mg_id)
        client = self._grpc_clients.get(mg)
        if client is None:
            client = AsyncGrpcClient(self.microgrid_grpc_url(mg))
            self._grpc_clients[mg] = client
        return client

    # --- reads: component-level (gRPC) ---------------------------------------

    async def components(self, mg_id: int | None = None) -> list[ComponentInfo]:
        """List the microgrid's components (id, category, name)."""
        return await self._grpc(mg_id).components()

    async def active_power(
        self, component_id: int, mg_id: int | None = None
    ) -> Power | None:
        """A component's active power — one sample off its gRPC stream."""
        watts = await self._grpc(mg_id).active_power(component_id)
        return None if watts is None else Power.from_watts(watts)

    async def soc(self, component_id: int, mg_id: int | None = None) -> Percentage | None:
        """A battery's state of charge — one sample off its gRPC stream."""
        pct = await self._grpc(mg_id).soc(component_id)
        return None if pct is None else Percentage.from_percent(pct)

    # --- reads: microgrid-level formula aggregates (HTTP) --------------------

    async def latest(self, mg_id: int | None = None) -> dict[str, Any]:
        """Latest sample per formula/component stream, keyed by name."""
        mg = self._resolve_mg(mg_id)
        return await self._http.get_json(f"/api/mg/{mg}/microgrid/latest")

    async def formula(self, name: str, mg_id: int | None = None) -> float | None:
        """Raw value of one formula stream (e.g. ``"grid_power"``), or None."""
        snap = (await self.latest(mg_id)).get(name)
        return None if snap is None else snap.get("value")

    async def metric_value(
        self, spec: MetricSpec[Q], mg_id: int | None = None
    ) -> Q | None:
        """Read one *aggregate* metric from the catalog, typed by its spec."""
        value = await self.formula(_STREAM_FOR[spec.name], mg_id)
        if value is None:
            return None
        return cast("Q", _FROM_WIRE[spec.quantity](value))

    async def grid_power(self, mg_id: int | None = None) -> Power | None:
        """Net power at the grid connection point (import positive)."""
        return await self.metric_value(_M.GRID_POWER, mg_id)

    async def pv_power(self, mg_id: int | None = None) -> Power | None:
        """Aggregate PV power (production negative)."""
        return await self.metric_value(_M.PV_POWER, mg_id)

    async def consumer_power(self, mg_id: int | None = None) -> Power | None:
        """Aggregate consumer (load) power."""
        return await self.metric_value(_M.CONSUMER_POWER, mg_id)

    async def battery_power(self, mg_id: int | None = None) -> Power | None:
        """Aggregate battery-pool power (discharge negative)."""
        return await self.metric_value(_M.BATTERY_POWER, mg_id)

    async def grid_energy(self, mg_id: int | None = None) -> Energy | None:
        """Cumulative net grid energy for the current run (import positive)."""
        return await self.metric_value(_M.GRID_ENERGY, mg_id)

    async def consumer_energy(self, mg_id: int | None = None) -> Energy | None:
        """Cumulative consumer (load) energy for the current run."""
        return await self.metric_value(_M.CONSUMER_ENERGY, mg_id)

    async def pv_energy(self, mg_id: int | None = None) -> Energy | None:
        """Cumulative PV energy for the current run (production negative)."""
        return await self.metric_value(_M.PV_ENERGY, mg_id)

    async def battery_energy(self, mg_id: int | None = None) -> Energy | None:
        """Cumulative net battery-pool energy for the current run (discharge
        negative)."""
        return await self.metric_value(_M.BATTERY_ENERGY, mg_id)

    # --- assertions (generic-first: pass a metric spec) ----------------------

    async def expect(
        self,
        spec: MetricSpec[Q],
        *,
        approx: Q | None = None,
        tol: Q | None = None,
        within: tuple[Q, Q] | None = None,
        max: Q | None = None,
        min: Q | None = None,
        for_: timedelta | None = None,
        timeout: timedelta = timedelta(seconds=10),
        poll: timedelta = timedelta(milliseconds=250),
        mg_id: int | None = None,
    ) -> Q | list[Q | None] | None:
        """Assert on an aggregate metric; its kind picks the semantics.

        ``await site.expect(GRID_POWER, max=...)`` settles;
        ``await site.expect(GRID_ENERGY, max=...)`` checks the total once.
        """
        bound: BoundMetric[Q] = spec.bind(
            functools.partial(self.metric_value, spec, mg_id)
        )
        return await expect_metric(
            bound,
            approx=approx,
            tol=tol,
            within=within,
            max=max,
            min=min,
            for_=for_,
            timeout=timeout,
            poll=poll,
        )

    # --- component handles ----------------------------------------------------

    def _component_id_of(self, target: Component | int) -> int:
        from ..build import Component

        if isinstance(target, Component):
            cid = target.args.get("id")
            if not isinstance(cid, int):
                raise ValueError(
                    "this call needs a component with an explicit id= to reference it"
                )
            return cid
        return int(target)

    def component(
        self, target: Component | int, mg_id: int | None = None
    ) -> ComponentHandle:
        """A handle onto one component (``site[id]`` is the same)."""
        return ComponentHandle(self, self._component_id_of(target), mg_id)

    def __getitem__(self, target: Component | int) -> ComponentHandle:
        return self.component(target)

    # --- writes: setpoints + bounds (gRPC — the real gateway) -----------------

    async def set_active_power(
        self,
        component_id: int,
        power: Power,
        *,
        lifetime: timedelta | None = None,
        mg_id: int | None = None,
    ) -> None:
        """Command an active-power setpoint; rejections raise, as production."""
        lifetime_s = lifetime.total_seconds() if lifetime is not None else None
        await self._grpc(mg_id).set_active_power(
            component_id, power.as_watts(), lifetime_s=lifetime_s
        )

    async def augment_bounds(
        self,
        component_id: int,
        lower: Power,
        upper: Power,
        mg_id: int | None = None,
    ) -> None:
        """Narrow a component's effective active-power bounds (TTL-limited)."""
        await self._grpc(mg_id).augment_active_power_bounds(
            component_id, lower.as_watts(), upper.as_watts()
        )

    # --- eval (the escape hatch) -----------------------------------------------

    async def eval(self, expr: str, mg_id: int | None = None) -> EvalResult:
        """Evaluate a raw Lisp form on the running interpreter."""
        return await self._http.eval(expr, mg_id)

    async def _eval_ok(self, expr: str, mg_id: int | None = None) -> EvalResult:
        """Eval and raise :class:`EvalRejected` on an interpreter rejection.

        The one choke point for programmatic eval — rejections can never
        silently no-op.
        """
        result = await self.eval(expr, mg_id)
        if not result.get("ok", True):
            raise EvalRejected(f"eval of {expr!r} failed: {result.get('error')}")
        return result

    # --- scenarios --------------------------------------------------------------

    def scenario(self, name: str) -> ScenarioRun:
        """Handle onto a registered ``(define-scenario …)`` for run/report."""
        return ScenarioRun(self, name)

    async def define_scenario(self, scenario: Scenario) -> ScenarioRun:
        """Register a Python-authored ``Scenario`` and return its ScenarioRun."""
        await self._eval_ok(scenario.to_lisp())
        return ScenarioRun(self, scenario.name)

    # --- generic polling ----------------------------------------------------------

    async def read_until(
        self,
        read: Callable[[], Awaitable[T]],
        predicate: Callable[[T], bool],
        *,
        timeout: timedelta = timedelta(seconds=10),
        poll: timedelta = timedelta(milliseconds=250),
    ) -> T:
        """Await ``read()`` repeatedly until ``predicate`` holds or timeout.

        Returns the last observed value either way (the caller asserts on it).
        """
        deadline = time.monotonic() + timeout.total_seconds()
        interval = poll.total_seconds()
        value = await read()
        while not predicate(value) and time.monotonic() < deadline:
            await asyncio.sleep(interval)
            value = await read()
        return value

    # --- lifecycle ------------------------------------------------------------------

    async def aclose(self) -> None:
        """Close the transports and stop a launched process."""
        for client in self._grpc_clients.values():
            await client.aclose()
        self._grpc_clients.clear()
        await self._http.aclose()
        terminate(self._process)

    async def __aenter__(self) -> Site:
        return self

    async def __aexit__(self, *_exc: object) -> None:
        await self.aclose()


class ComponentHandle:
    """A component in a running site, acted on by intent (async)."""

    def __init__(self, site: Site, component_id: int, mg_id: int | None = None) -> None:
        self._site = site
        self._id = component_id
        self._mg = mg_id

    async def command(
        self,
        *,
        active_power: Power | None = None,
        bounds: tuple[Power, Power] | None = None,
        lifetime: timedelta | None = None,
    ) -> None:
        """Issue a control command as the app would — through the gRPC gateway."""
        if active_power is not None:
            await self._site.set_active_power(
                self._id, active_power, lifetime=lifetime, mg_id=self._mg
            )
        if bounds is not None:
            await self._site.augment_bounds(
                self._id, bounds[0], bounds[1], mg_id=self._mg
            )

    async def status(
        self,
        *,
        health: Health | None = None,
        command_mode: CommandMode | None = None,
        telemetry_mode: TelemetryMode | None = None,
    ) -> None:
        """Set the component's operational status (fault injection)."""
        if health is not None:
            await self._site._eval_ok(
                f"(set-component-health {self._id} {to_lisp_atom(health)})", self._mg
            )
        if command_mode is not None:
            mode = to_lisp_atom(command_mode)
            await self._site._eval_ok(
                f"(set-component-command-mode {self._id} {mode})", self._mg
            )
        if telemetry_mode is not None:
            mode = to_lisp_atom(telemetry_mode)
            await self._site._eval_ok(
                f"(set-component-telemetry-mode {self._id} {mode})", self._mg
            )

    async def drive(
        self,
        *,
        power: Power | RawLisp | None = None,
        sunlight: Percentage | None = None,
    ) -> None:
        """Drive the environment: a meter's published power, a PV's sunlight."""
        if power is not None:
            await self._site._eval_ok(
                f"(set-meter-power {self._id} {to_lisp_atom(power)})", self._mg
            )
        if sunlight is not None:
            await self._site._eval_ok(
                f"(set-solar-sunlight {self._id} {to_lisp_atom(sunlight)})", self._mg
            )

    async def active_power(self) -> Power | None:
        """A single sample of this component's active power (gRPC)."""
        return await self._site.active_power(self._id, self._mg)

    async def soc(self) -> Percentage | None:
        """A single sample of this battery's state of charge (gRPC)."""
        return await self._site.soc(self._id, self._mg)

    async def expect(
        self,
        spec: MetricSpec[Q],
        *,
        approx: Q | None = None,
        tol: Q | None = None,
        within: tuple[Q, Q] | None = None,
        max: Q | None = None,
        min: Q | None = None,
        for_: timedelta | None = None,
        timeout: timedelta = timedelta(seconds=10),
        poll: timedelta = timedelta(milliseconds=250),
    ) -> Q | list[Q | None] | None:
        """Assert on one of this component's metrics (``ACTIVE_POWER``, ``SOC``)."""
        reads: dict[str, Callable[[], Awaitable[Any]]] = {
            "active_power": self.active_power,
            "soc": self.soc,
        }
        untyped_read = reads.get(spec.name)
        if untyped_read is None:
            raise ValueError(
                f"component expect supports active_power / soc, not {spec.name!r}"
            )
        read = cast("Callable[[], Awaitable[Q | None]]", untyped_read)
        bound = spec.bind(read, label=f"component {self._id} {spec.name}")
        return await expect_metric(
            bound,
            approx=approx,
            tol=tol,
            within=within,
            max=max,
            min=min,
            for_=for_,
            timeout=timeout,
            poll=poll,
        )


class ScenarioRun:
    """A registered scenario bound to a running site (async)."""

    def __init__(self, site: Site, name: str) -> None:
        self._site = site
        self._name = name

    async def _length_s(self) -> float | None:
        for scenario in await self._site._http.get_json("/api/scenarios"):
            if scenario.get("name") == self._name:
                return scenario.get("length_s")
        return None

    async def run(
        self,
        *,
        wait: bool = True,
        until: timedelta | None = None,
        poll: timedelta = timedelta(seconds=1),
    ) -> ScenarioRun:
        """Start the scenario; with ``wait`` block until it finishes, stop it."""
        length = until.total_seconds() if until is not None else await self._length_s()
        # Resolve the wait length BEFORE starting, so an unwaitable scenario
        # fails fast instead of being left running with nothing to stop it.
        if wait and length is None:
            raise ValueError(
                f"scenario {self._name!r} has no :length; pass until= to bound the wait"
            )
        await self._site._http.post(f"/api/scenarios/{self._name}/start")
        if not wait or length is None:
            return self
        deadline = time.monotonic() + length + 5.0
        interval = poll.total_seconds()
        while time.monotonic() < deadline:
            state = await self._site._http.get_json("/api/scenario")
            if state.get("ended_at") is not None:
                break
            if (state.get("elapsed_s") or 0.0) >= length:
                break
            await asyncio.sleep(interval)
        await self._site._http.post("/api/scenarios/stop")
        return self

    async def report(self) -> ScenarioReport:
        """The parsed scenario report (pass/fail ledger + peak/soc stats)."""
        return await self._site._http.get_json("/api/scenario/report")

    async def assert_passed(self) -> ScenarioReport:
        """Raise if any ``(check …)`` failed; return the report otherwise."""
        report = await self.report()
        failed = report.get("checks_failed", 0)
        if failed:
            broken = [c for c in report.get("checks", []) if not c.get("passed", True)]
            raise AssertionError(
                f"scenario {self._name!r}: {failed} check(s) failed: {broken}"
            )
        return report

    async def events(self, *, since: int = 0) -> list[JournalEvent]:
        """The scenario's journal events (list of ``{kind, payload, …}``)."""
        body = await self._site._http.get_json(f"/api/scenario/events?since={since}")
        return body.get("events", [])


def connect(
    *,
    ui: str,
    microgrids: dict[int, MicrogridEndpoint] | None = None,
    assets: str | None = None,
    dispatch: str | None = None,
) -> Site:
    """Attach to an already-running switchyard (no process is spawned)."""
    return Site(
        ui=ui,
        microgrids=microgrids or {},
        assets=assets,
        dispatch=dispatch,
        process=None,
    )


@asynccontextmanager
async def launch(
    config: LaunchConfig,
    *,
    bin: str | os.PathLike[str] | None = None,
    ready_timeout: timedelta = timedelta(seconds=20),
) -> AsyncIterator[Site]:
    """Boot switchyard on ephemeral ports and yield a ready async ``Site``.

    Same contract as the sync ``switchyard.launch``, but the handshake wait
    awaits instead of blocking, and the site is torn down on exit::

        async with sw.aio.launch(topology) as site:
            ...
    """
    spawned = spawn_switchyard(config, bin)
    site: Site | None = None
    try:
        deadline = time.monotonic() + ready_timeout.total_seconds()
        while True:
            if (
                spawned.endpoints_file.exists()
                and spawned.endpoints_file.stat().st_size > 0
            ):
                break
            if spawned.process.poll() is not None:
                spawned.fail(
                    RuntimeError,
                    f"switchyard exited early (code {spawned.process.returncode}) "
                    f"before emitting endpoints:",
                )
            if time.monotonic() >= deadline:
                spawned.fail(
                    TimeoutError,
                    f"switchyard did not emit endpoints within {ready_timeout}:",
                )
            await asyncio.sleep(0.1)
        endpoints = json.loads(spawned.endpoints_file.read_text())
        microgrids = {
            int(m["id"]): MicrogridEndpoint(
                id=int(m["id"]), name=m["name"], grpc=m["grpc"]
            )
            for m in endpoints.get("microgrids", [])
        }
        site = Site(
            ui=endpoints["ui"],
            microgrids=microgrids,
            assets=endpoints.get("assets"),
            dispatch=endpoints.get("dispatch"),
            process=spawned.process,
        )
        yield site
    finally:
        if site is not None:
            await site.aclose()
        else:
            terminate(spawned.process)
