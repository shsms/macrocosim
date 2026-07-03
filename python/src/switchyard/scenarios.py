"""Run registered Lisp scenarios from Python and gate on their checks.

A scenario is defined in the loaded config with ``(define-scenario …)``;
reach it via ``site.scenario(name)``:

    report = site.scenario("cloud-fade").run(wait=True).assert_passed()

``run(wait=True)`` starts it live, blocks until it finishes (its
``:length`` or ``until=``), then stops it so the report freezes — the same
sequence as ``swctl scenario run --wait``. ``report()`` returns the parsed
pass/fail ledger + stats; ``assert_passed()`` raises on any failed
``(check …)``; ``events()`` reads the journal.

Times are :mod:`datetime` (a ``timedelta`` offset, or a ``datetime.time``
for an absolute schedule); check values are ``frequenz-quantities``.
"""

from __future__ import annotations

import json
import os
import subprocess
import tempfile
import time
from dataclasses import dataclass, field
from datetime import time as clock_time
from datetime import timedelta
from pathlib import Path
from typing import TYPE_CHECKING, Any, TypeAlias

from .build import ConfigSource, RawLisp, to_lisp_atom
from .enums import Metric, Schedule
from .runtime import _render_config, _resolve_binary

if TYPE_CHECKING:
    from frequenz.quantities import Power, Quantity

    from ._http import HttpClient
    from .runtime import Site

# A scenario report / journal are dynamic JSON from switchyard; named for intent.
ScenarioReport: TypeAlias = dict[str, Any]
JournalEvent: TypeAlias = dict[str, Any]


def _time_literal(value: timedelta | clock_time) -> str:
    """A scenario time as Lisp: a ``timedelta`` becomes a ``"<seconds>s"`` offset,
    a ``datetime.time`` an ``"HH:MM:SS"`` clock string (for absolute schedules).

    Whole seconds render without a decimal point, fractional as a plain decimal
    — never scientific notation, since switchyard's ``parse-offset`` splits at
    the first letter and would read an ``e``-exponent as the unit.
    """
    if isinstance(value, clock_time):
        return f'"{value.isoformat()}"'
    secs = value.total_seconds()
    text = f"{secs:.0f}" if secs == int(secs) else repr(secs)
    return f'"{text}s"'


@dataclass
class Check:
    """A timed ``(check …)`` assertion within a scenario."""

    at: timedelta | clock_time
    component: int
    metric: Metric
    approx: Quantity | None = None
    tol: Quantity | None = None
    min: Quantity | None = None
    max: Quantity | None = None

    def to_lisp(self) -> str:
        parts = [
            _time_literal(self.at),
            f":component {self.component}",
            f":metric {to_lisp_atom(self.metric)}",
        ]
        for key in ("approx", "tol", "min", "max"):
            value = getattr(self, key)
            if value is not None:
                parts.append(f":{key} {to_lisp_atom(value)}")
        return f"(check {' '.join(parts)})"


@dataclass
class Scenario:
    """Author a scenario in Python; renders to ``(define-scenario …)``.

    Register it on a running site with ``site.define_scenario(scenario)``,
    which returns a :class:`ScenarioRun` to ``run(...)`` and gate. ``check``
    adds a timed assertion (the CI gate); ``drive_meter`` installs a
    continuous meter source; ``drive`` / ``cue`` splice a raw Lisp
    ``(drive-* …)`` / ``(at …)`` form for anything not modelled here.
    """

    name: str
    schedule: Schedule = Schedule.RELATIVE
    length: timedelta | None = None
    seed: int | None = None
    description: str | None = None
    _checks: list[Check] = field(default_factory=list)
    _drives: list[str] = field(default_factory=list)
    _cues: list[str] = field(default_factory=list)

    def check(
        self,
        at: timedelta | clock_time,
        *,
        component: int,
        metric: Metric,
        approx: Quantity | None = None,
        tol: Quantity | None = None,
        min: Quantity | None = None,
        max: Quantity | None = None,
    ) -> Scenario:
        self._checks.append(Check(at, component, metric, approx, tol, min, max))
        return self

    def drive_meter(self, component: int, value: Power | RawLisp) -> Scenario:
        self._drives.append(f"(drive-meter {component} {to_lisp_atom(value)})")
        return self

    def drive(self, form: str | RawLisp) -> Scenario:
        """Splice a raw ``(drive-* …)`` form (e.g. drive-solar with a timeline)."""
        self._drives.append(form.text if isinstance(form, RawLisp) else form)
        return self

    def cue(self, at: timedelta | clock_time, action: str | RawLisp) -> Scenario:
        """A timed cue: ``(at "<time>" <action>)``."""
        text = action.text if isinstance(action, RawLisp) else action
        self._cues.append(f"(at {_time_literal(at)} {text})")
        return self

    def to_lisp(self) -> str:
        parts = [f":name {to_lisp_atom(self.name)}"]
        if self.description is not None:
            parts.append(f":description {to_lisp_atom(self.description)}")
        parts.append(f":schedule {to_lisp_atom(self.schedule)}")
        if self.length is not None:
            parts.append(f":length {_time_literal(self.length)}")
        if self.seed is not None:
            parts.append(f":seed {self.seed}")
        if self._drives:
            parts.append(f":drive (list {' '.join(self._drives)})")
        if self._cues:
            parts.append(f":cues (list {' '.join(self._cues)})")
        if self._checks:
            checks = " ".join(c.to_lisp() for c in self._checks)
            parts.append(f":expect (list {checks})")
        return f"(define-scenario {' '.join(parts)})"


class ScenarioRun:
    """A registered scenario bound to a running site."""

    def __init__(self, site: Site, name: str) -> None:
        self._site = site
        self._name = name

    @property
    def _http(self) -> HttpClient:
        return self._site._http

    def _length_s(self) -> float | None:
        for scenario in self._http.get_json("/api/scenarios"):
            if scenario.get("name") == self._name:
                return scenario.get("length_s")
        return None

    def run(
        self,
        *,
        wait: bool = True,
        until: timedelta | None = None,
        poll: timedelta = timedelta(seconds=1),
    ) -> ScenarioRun:
        """Start the scenario; with ``wait`` block until it finishes, stop it."""
        length = until.total_seconds() if until is not None else self._length_s()
        # Resolve the wait length BEFORE starting, so an unwaitable scenario
        # fails fast instead of being left running with nothing to stop it.
        if wait and length is None:
            raise ValueError(
                f"scenario {self._name!r} has no :length; pass until= to bound the wait"
            )
        self._http.post(f"/api/scenarios/{self._name}/start")
        if not wait or length is None:
            return self
        deadline = time.monotonic() + length + 5.0
        interval = poll.total_seconds()
        while time.monotonic() < deadline:
            state = self._http.get_json("/api/scenario")
            if state.get("ended_at") is not None:
                break
            if (state.get("elapsed_s") or 0.0) >= length:
                break
            time.sleep(interval)
        self._http.post("/api/scenarios/stop")
        return self

    def report(self) -> ScenarioReport:
        """The parsed scenario report (pass/fail ledger + peak/soc stats)."""
        return self._http.get_json("/api/scenario/report")

    def assert_passed(self) -> ScenarioReport:
        """Raise if any ``(check …)`` failed; return the report otherwise."""
        report = self.report()
        failed = report.get("checks_failed", 0)
        if failed:
            broken = [c for c in report.get("checks", []) if not c.get("passed", True)]
            raise AssertionError(
                f"scenario {self._name!r}: {failed} check(s) failed: {broken}"
            )
        return report

    def events(self, *, since: int = 0) -> list[JournalEvent]:
        """The scenario's journal events (list of ``{kind, payload, …}``)."""
        body = self._http.get_json(f"/api/scenario/events?since={since}")
        return body.get("events", [])


def run_scenario_stepped(
    config: ConfigSource,
    name: str,
    *,
    swctl_bin: str | os.PathLike[str] | None = None,
    until: timedelta | None = None,
    step: int | None = None,
    assert_pass: bool = True,
) -> ScenarioReport:
    """Run a scenario headless on the stepped clock and return its report.

    Deterministic and faster than real time — no server, no app under test
    (e2e-testing.md mode 1). ``config`` is a ``.lisp`` path, or builder
    object(s) (a ``Microgrid`` and ``Scenario``) rendered to a temp config.
    Shells ``swctl scenario run NAME --stepped --config … --json``; with
    ``assert_pass`` a non-zero exit (a failed ``(check …)``) raises.
    """
    binary = _resolve_binary(
        "swctl", env_var="SWCTL_BIN", explicit=swctl_bin, flag="swctl_bin"
    )
    tmpdir = Path(tempfile.mkdtemp(prefix="switchyard-py-"))
    config_path = _render_config(config, tmpdir)
    args = [
        binary,
        "scenario",
        "run",
        name,
        "--stepped",
        "--config",
        str(config_path),
        "--json",
    ]
    if until is not None:
        args += ["--until", str(int(until.total_seconds()))]
    if step is not None:
        args += ["--step", str(int(step))]
    if assert_pass:
        args += ["--assert"]

    result = subprocess.run(args, capture_output=True, text=True, check=False)
    report: ScenarioReport | None
    try:
        report = json.loads(result.stdout)
    except json.JSONDecodeError:
        report = None
    if assert_pass and result.returncode != 0:
        broken = (
            [c for c in report.get("checks", []) if not c.get("passed", True)]
            if report
            else result.stderr.strip()
        )
        raise AssertionError(
            f"stepped scenario {name!r} failed (exit {result.returncode}): {broken}"
        )
    if report is None:
        raise RuntimeError(f"swctl produced no JSON report:\n{result.stderr}")
    return report


__all__ = [
    "Check",
    "Scenario",
    "ScenarioRun",
    "ScenarioReport",
    "JournalEvent",
    "run_scenario_stepped",
]
