"""Process lifecycle and the ``Site`` handle onto a running switchyard.

``launch()`` boots the ``switchyard`` binary on ephemeral ports, waits for
the endpoint-emission handshake, and hands back a ``Site`` (a context
manager); ``connect()`` attaches to an already-running instance. ``Site``
is the handle through which tests read (component telemetry over gRPC,
microgrid aggregates over HTTP), mutate and assert via component handles, and
run scenarios. Reads return typed ``frequenz-quantities`` (``Power`` /
``Percentage``); writes take them too.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sysconfig
import tempfile
import time
from collections.abc import Callable
from dataclasses import dataclass
from datetime import timedelta
from pathlib import Path
from typing import TYPE_CHECKING, Any, TypeVar

from frequenz.quantities import Energy, Percentage, Power

from ._http import EvalResult, HttpClient
from .build import ConfigSource, LaunchConfig, LispRenderable

if TYPE_CHECKING:
    from ._grpc import ComponentInfo, GrpcClient
    from .build import Component
    from .handles import ComponentHandle, MicrogridExpect, MicrogridHandle
    from .scenarios import Scenario, ScenarioRun

T = TypeVar("T")


@dataclass(frozen=True)
class MicrogridEndpoint:
    """One microgrid's resolved gRPC address."""

    id: int
    name: str
    grpc: str  # "host:port" of the Microgrid gRPC API


class Site:
    """A running switchyard instance and its resolved endpoints.

    Use as a context manager; exiting tears the process down. Reads exposed
    here are the graph-derived formula aggregates (grid / consumer / PV /
    battery power) served over HTTP.
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
        self._http = HttpClient(f"http://{ui}")
        self._grpc_clients: dict[int, GrpcClient] = {}

    @property
    def grpc(self) -> str:
        """``host:port`` of the first (default) microgrid's gRPC API."""
        return next(iter(self.microgrids.values())).grpc

    @property
    def grpc_url(self) -> str:
        """The default microgrid's gRPC API as a ``grpc://host:port`` URL.

        The scheme-prefixed form a gRPC client connects with — e.g.
        ``microgrid.initialize(server_url=site.grpc_url)``.
        """
        return f"grpc://{self.grpc}"

    def microgrid_grpc_url(self, mg_id: int) -> str:
        """A specific microgrid's gRPC API as a ``grpc://host:port`` URL."""
        return f"grpc://{self.microgrids[mg_id].grpc}"

    def _resolve_mg(self, mg_id: int | None) -> int:
        if mg_id is not None:
            return mg_id
        return next(iter(self.microgrids))

    def grpc_client(self, mg_id: int | None = None) -> GrpcClient:
        """The gRPC client for a microgrid, connected on first use.

        Requires the ``grpc`` extra (``frequenz-client-microgrid``). One
        cached connection per microgrid — the same client the app under
        test would open against switchyard.
        """
        mg = self._resolve_mg(mg_id)
        client = self._grpc_clients.get(mg)
        if client is None:
            from ._grpc import GrpcClient

            client = GrpcClient(self.microgrid_grpc_url(mg))
            self._grpc_clients[mg] = client
        return client

    # --- reads: component-level (gRPC — what the app under test sees) ------

    def components(self, mg_id: int | None = None) -> list[ComponentInfo]:
        """List the microgrid's components (id, category, name)."""
        return self.grpc_client(mg_id).components()

    def active_power(self, component_id: int, mg_id: int | None = None) -> Power | None:
        """A component's active power — one sample off its gRPC stream."""
        watts = self.grpc_client(mg_id).active_power(component_id)
        return None if watts is None else Power.from_watts(watts)

    def soc(self, component_id: int, mg_id: int | None = None) -> Percentage | None:
        """A battery's state of charge — one sample off its gRPC stream."""
        pct = self.grpc_client(mg_id).soc(component_id)
        return None if pct is None else Percentage.from_percent(pct)

    # --- fluent runtime mutation ------------------------------------------

    def _component_id_of(self, target: Component | int) -> int:
        """Resolve a builder Component (with explicit id) or int to an id."""
        from .build import Component

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
        """A handle onto one component (``site[id]`` is the same): ``command`` /
        ``status`` / ``drive``, reads (``active_power`` / ``soc``), and
        ``expect`` (assertions)."""
        from .handles import ComponentHandle

        return ComponentHandle(self, self._component_id_of(target), mg_id)

    def __getitem__(self, target: Component | int) -> ComponentHandle:
        return self.component(target)

    @property
    def expect(self) -> MicrogridExpect:
        """Settle-aware assertions on the default microgrid's graph aggregates
        (``grid_power`` / ``pv_power`` / ``consumer_power`` / ``battery_power``)."""
        from .handles import MicrogridExpect

        return MicrogridExpect(self, None)

    def microgrid(self, mg_id: int) -> MicrogridHandle:
        """A handle onto one microgrid (for multi-microgrid setups): ``at`` its
        components, ``expect`` its aggregates, read its formulas."""
        from .handles import MicrogridHandle

        return MicrogridHandle(self, mg_id)

    # --- writes: setpoints + bounds (gRPC — exercises the real gateway) ---

    def set_active_power(
        self,
        component_id: int,
        power: Power,
        *,
        lifetime: timedelta | None = None,
        mg_id: int | None = None,
    ) -> None:
        """Command a component's active-power setpoint; errors if the value is
        outside the live envelope, exactly as production does."""
        lifetime_s = lifetime.total_seconds() if lifetime is not None else None
        self.grpc_client(mg_id).set_active_power(
            component_id, power.as_watts(), lifetime_s=lifetime_s
        )

    def augment_bounds(
        self,
        component_id: int,
        lower: Power,
        upper: Power,
        mg_id: int | None = None,
    ) -> None:
        """Narrow a component's effective active-power bounds (TTL-limited)."""
        self.grpc_client(mg_id).augment_active_power_bounds(
            component_id, lower.as_watts(), upper.as_watts()
        )

    # --- reads: microgrid-level formula aggregates (HTTP) -----------------

    def latest(self, mg_id: int | None = None) -> dict[str, Any]:
        """Latest sample per formula/component stream, keyed by name."""
        mg = self._resolve_mg(mg_id)
        return self._http.get_json(f"/api/mg/{mg}/microgrid/latest")

    def formula(self, name: str, mg_id: int | None = None) -> float | None:
        """Raw value of one formula stream (e.g. ``"grid_power"``), or None."""
        snap = self.latest(mg_id).get(name)
        return None if snap is None else snap.get("value")

    def _power_formula(self, name: str, mg_id: int | None) -> Power | None:
        value = self.formula(name, mg_id)
        return None if value is None else Power.from_watts(value)

    def grid_power(self, mg_id: int | None = None) -> Power | None:
        return self._power_formula("grid_power", mg_id)

    def pv_power(self, mg_id: int | None = None) -> Power | None:
        return self._power_formula("pv_power", mg_id)

    def consumer_power(self, mg_id: int | None = None) -> Power | None:
        return self._power_formula("consumer_power", mg_id)

    def battery_power(self, mg_id: int | None = None) -> Power | None:
        return self._power_formula("battery_pool_power", mg_id)

    # --- reads: cumulative energy (integrated server-side from the power
    # aggregates; signed like the power, so net across the bus) ------------

    def _energy_formula(self, name: str, mg_id: int | None) -> Energy | None:
        value = self.formula(name, mg_id)
        return None if value is None else Energy.from_watt_hours(value)

    def grid_energy(self, mg_id: int | None = None) -> Energy | None:
        """Cumulative net grid energy for the current run (import positive).

        A config hot-reload resets the site and starts a new run, so the
        total restarts at zero there — not only at launch.
        """
        return self._energy_formula("grid_energy", mg_id)

    def consumer_energy(self, mg_id: int | None = None) -> Energy | None:
        """Cumulative consumer (load) energy for the current run."""
        return self._energy_formula("consumer_energy", mg_id)

    def pv_energy(self, mg_id: int | None = None) -> Energy | None:
        """Cumulative PV energy for the current run (production negative)."""
        return self._energy_formula("pv_energy", mg_id)

    def battery_energy(self, mg_id: int | None = None) -> Energy | None:
        """Cumulative net battery-pool energy for the current run (discharge
        negative)."""
        return self._energy_formula("battery_pool_energy", mg_id)

    def eval(self, expr: str, mg_id: int | None = None) -> EvalResult:
        """Evaluate a raw Lisp form on the running interpreter."""
        return self._http.eval(expr, mg_id)

    def scenario(self, name: str) -> ScenarioRun:
        """Handle onto a registered ``(define-scenario …)`` for run/report."""
        from .scenarios import ScenarioRun

        return ScenarioRun(self, name)

    def define_scenario(self, scenario: Scenario) -> ScenarioRun:
        """Register a Python-authored ``Scenario`` and return its ScenarioRun."""
        from .scenarios import ScenarioRun

        result = self.eval(scenario.to_lisp())
        if not result.get("ok", True):
            raise ValueError(f"define-scenario failed: {result.get('error')}")
        return ScenarioRun(self, scenario.name)

    def read_until(
        self,
        read: Callable[[], T],
        predicate: Callable[[T], bool],
        *,
        timeout: timedelta = timedelta(seconds=10),
        poll: timedelta = timedelta(milliseconds=250),
    ) -> T:
        """Poll ``read()`` until ``predicate`` holds or ``timeout`` elapses.

        Returns the last observed value either way (the caller asserts on
        it). The live sim is real-time, so settle-then-check with a
        tolerance rather than reading a single transient sample.
        """
        deadline = time.monotonic() + timeout.total_seconds()
        interval = poll.total_seconds()
        value = read()
        while not predicate(value) and time.monotonic() < deadline:
            time.sleep(interval)
            value = read()
        return value

    # --- lifecycle --------------------------------------------------------

    def close(self) -> None:
        for client in self._grpc_clients.values():
            client.close()
        self._grpc_clients.clear()
        self._http.close()
        _terminate(self._process)

    def __enter__(self) -> Site:
        return self

    def __exit__(self, *_exc: object) -> None:
        self.close()


def which_binary(name: str) -> str | None:
    """Find ``name`` on PATH, or in this interpreter's scripts directory.

    A platform wheel bundles the binaries there (``<venv>/bin``), which is
    reachable even when that directory isn't on PATH (unactivated venv, some
    test runners); ``shutil.which`` also resolves the ``.exe`` on Windows.
    """
    return shutil.which(name) or shutil.which(name, path=sysconfig.get_path("scripts"))


def _resolve_binary(
    name: str,
    *,
    env_var: str,
    explicit: str | os.PathLike[str] | None,
    flag: str,
) -> str:
    """Locate a bundled binary: an explicit path → ``$env_var`` → PATH / wheel.

    ``flag`` names the launch/run keyword that overrides it, for the not-found
    message (``bin`` for switchyard, ``swctl_bin`` for swctl).
    """
    for candidate in (explicit, os.environ.get(env_var)):
        if candidate:
            path = Path(candidate)
            if path.is_file():
                return str(path)
            raise FileNotFoundError(f"{name} binary not found at {path}")
    found = which_binary(name)
    if found:
        return found
    raise FileNotFoundError(
        f"no {name} binary; install the switchyard platform wheel, pass "
        f"{flag}=..., set {env_var}, or put it on PATH"
    )


def _render_config(config: ConfigSource, tmpdir: Path) -> Path:
    """A config file path as-is, or render to_lisp object(s) to a temp file."""
    if isinstance(config, LispRenderable):
        forms: list[LispRenderable] = [config]
    elif isinstance(config, (str, os.PathLike)):
        # config is str | PathLike here; the structural narrowing leaves a
        # spurious Protocol-and-PathLike intersection the checker can't shed.
        return Path(config)  # ty: ignore[invalid-argument-type]
    else:
        forms = list(config)
    path = tmpdir / "config.lisp"
    path.write_text("\n".join(f.to_lisp() for f in forms) + "\n")
    return path


def _site_from_endpoints(
    endpoints: dict[str, Any], process: subprocess.Popen[bytes] | None
) -> Site:
    microgrids = {
        int(m["id"]): MicrogridEndpoint(id=int(m["id"]), name=m["name"], grpc=m["grpc"])
        for m in endpoints.get("microgrids", [])
    }
    return Site(
        ui=endpoints["ui"],
        microgrids=microgrids,
        assets=endpoints.get("assets"),
        dispatch=endpoints.get("dispatch"),
        process=process,
    )


def launch(
    config: LaunchConfig,
    *,
    bin: str | os.PathLike[str] | None = None,
    ready_timeout: timedelta = timedelta(seconds=20),
) -> Site:
    """Boot switchyard on ephemeral ports and return a ready ``Site``.

    ``config`` is a path to a ``.lisp`` file, or a builder object with a
    ``to_lisp()`` method (a :class:`switchyard.build.Microgrid`), which is
    rendered to a temp config first. Spawns ``switchyard <config>
    --ephemeral-ports --emit-endpoints=<file>`` and blocks until the
    endpoints file is written (the readiness signal). Raises if the process
    dies first or the handshake times out.
    """
    binary = _resolve_binary(
        "switchyard", env_var="SWITCHYARD_BIN", explicit=bin, flag="bin"
    )
    tmpdir = Path(tempfile.mkdtemp(prefix="switchyard-py-"))
    config_path = _render_config(config, tmpdir)
    endpoints_file = tmpdir / "endpoints.json"
    log_file = tmpdir / "switchyard.log"

    # Send the child's output to a file, not a PIPE: an undrained pipe fills
    # its ~64KB buffer and the child blocks on write before it can emit the
    # endpoints handshake. The file also carries diagnostics for failures.
    with log_file.open("wb") as log:
        process = subprocess.Popen(
            [
                binary,
                str(config_path),
                "--ephemeral-ports",
                f"--emit-endpoints={endpoints_file}",
            ],
            stdout=log,
            stderr=subprocess.STDOUT,
        )

    def _fail(exc_type: type[Exception], message: str) -> None:
        _terminate(process)
        tail = log_file.read_text(errors="replace")[-4000:]
        raise exc_type(f"{message}\n{tail}")

    deadline = time.monotonic() + ready_timeout.total_seconds()
    while True:
        if endpoints_file.exists() and endpoints_file.stat().st_size > 0:
            break
        if process.poll() is not None:
            _fail(
                RuntimeError,
                f"switchyard exited early (code {process.returncode}) before "
                f"emitting endpoints:",
            )
        if time.monotonic() >= deadline:
            _fail(
                TimeoutError,
                f"switchyard did not emit endpoints within {ready_timeout}:",
            )
        time.sleep(0.1)

    endpoints = json.loads(endpoints_file.read_text())
    return _site_from_endpoints(endpoints, process)


def _terminate(process: subprocess.Popen[bytes] | None) -> None:
    """Stop a child process, escalating to kill if terminate doesn't take."""
    if process is None or process.poll() is not None:
        return
    process.terminate()
    try:
        process.wait(timeout=5.0)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=5.0)


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
