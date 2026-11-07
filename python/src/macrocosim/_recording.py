"""Recording a microgrid: the `Recording` that `Microgrid.record` returns, and
the `RecordingReport` its stop answers.

Internal; imports `_errors` and `_units`, and `_session` for typing only.
"""

from __future__ import annotations

import logging
from dataclasses import dataclass
from datetime import datetime
from types import TracebackType
from typing import TYPE_CHECKING, Any, Literal, Protocol

from ._errors import Rejected, ServerError
from ._units import check_type

if TYPE_CHECKING:
    from ._session import _Session

_log = logging.getLogger("macrocosim")


EndedBy = Literal["stop", "rebuild"]
"""What ended a recording: its own stop, or a rebuild of the microgrid."""


@dataclass(frozen=True)
class BatteryEnergy:
    """The energy one battery took and gave during a recording."""

    id: int
    charge_wh: float
    discharge_wh: float


@dataclass(frozen=True)
class PvEnergy:
    """The energy one solar inverter produced during a recording."""

    id: int
    produced_wh: float


@dataclass(frozen=True)
class SocStats:
    """The state of charge across the microgrid's batteries when the report was
    made: mean, median, and the most common whole percent."""

    mean_pct: float
    median_pct: float
    mode_pct: int | None


@dataclass(frozen=True)
class WindowAverage:
    """The mean grid power over one 15-minute UTC window."""

    window_start: datetime
    avg_w: float


@dataclass(frozen=True)
class RecordingReport:
    """What a recording measured, as the simulator's recording stop answers it.

    `ended_by` is ``"stop"`` for a recording its block stopped, and
    ``"rebuild"`` for one a rebuild of the microgrid (a UI edit or an undo)
    ended early.
    """

    name: str | None
    elapsed_s: float
    ended_by: EndedBy | None
    peak_grid_w: float
    peak_grid_var: float
    site_pf_at_reactive_peak: float | None
    total_battery_charged_wh: float
    total_battery_discharged_wh: float
    total_pv_produced_wh: float
    per_battery: tuple[BatteryEnergy, ...]
    per_pv: tuple[PvEnergy, ...]
    soc_stats: SocStats | None
    grid_window_averages: tuple[WindowAverage, ...]

    @classmethod
    def _from_json(cls, body: Any) -> RecordingReport:
        ended_by = body["ended_by"]
        if ended_by not in ("stop", "rebuild", None):
            raise ValueError(f"unknown ended_by {ended_by!r}")
        soc = body["soc_stats"]
        pf = body["site_pf_at_reactive_peak"]
        return cls(
            name=None if body["name"] is None else str(body["name"]),
            elapsed_s=float(body["elapsed_s"]),
            ended_by=ended_by,
            peak_grid_w=float(body["peak_grid_w"]),
            peak_grid_var=float(body["peak_grid_var"]),
            site_pf_at_reactive_peak=None if pf is None else float(pf),
            total_battery_charged_wh=float(body["total_battery_charged_wh"]),
            total_battery_discharged_wh=float(body["total_battery_discharged_wh"]),
            total_pv_produced_wh=float(body["total_pv_produced_wh"]),
            per_battery=tuple(
                BatteryEnergy(
                    int(b["id"]), float(b["charge_wh"]), float(b["discharge_wh"])
                )
                for b in body["per_battery"]
            ),
            per_pv=tuple(
                PvEnergy(int(p["id"]), float(p["produced_wh"])) for p in body["per_pv"]
            ),
            soc_stats=None
            if soc is None
            else SocStats(
                float(soc["mean_pct"]),
                float(soc["median_pct"]),
                None if soc["mode_pct"] is None else int(soc["mode_pct"]),
            ),
            grid_window_averages=tuple(
                WindowAverage(
                    datetime.fromisoformat(w["window_start"]), float(w["avg_w"])
                )
                for w in body["grid_window_averages"]
            ),
        )


class _Host(Protocol):
    """The microgrid a recording belongs to: it holds the open one."""

    _recording: Recording | None


class Recording:
    """A recording of one microgrid, from `Microgrid.record`.

    An async context manager: entering starts the recording, and leaving the
    block stops it, normally or with an exception. Add named events with
    `event`; after the block, `report` gives what it measured.

    A UI edit or an undo that rebuilds the microgrid ends the recording early.
    The block goes on, unrecorded from the rebuild on: `event` logs a warning
    and drops the event, leaving the block logs a warning, and the report says
    ``ended_by="rebuild"``. A microgrid unloaded during the block leaves no
    report: leaving it logs a warning, and `report` raises.
    """

    def __init__(self, session: _Session, host: _Host, name: str, csv: bool) -> None:
        self._session = session
        self._host = host
        self._name = name
        check_type(f"{self._label} name", name, str)
        check_type(f"{self._label} csv", csv, bool)
        self._csv = csv
        self._check_free()
        self._open = False
        self._entered = False
        self._report: RecordingReport | None = None
        self._no_report = "its block has not exited"

    @property
    def _label(self) -> str:
        return f"Microgrid {self._session.microgrid_id} record({self._name!r})"

    def _check_free(self) -> None:
        if self._host._recording is not None:
            raise RuntimeError(
                f"Microgrid {self._session.microgrid_id} already has a recording "
                f"open ({self._host._recording._name!r}); leave its block first"
            )

    async def __aenter__(self) -> Recording:
        if self._entered:
            raise RuntimeError(f"{self._label} was already entered; record again")
        self._entered = True
        self._check_free()
        self._host._recording = self
        try:
            await self._session.post_json(
                self._session.microgrid_path("recording/start"),
                {"name": self._name, "csv": self._csv},
                call=self._label,
            )
        except BaseException:
            self._host._recording = None
            raise
        self._open = True
        return self

    async def __aexit__(
        self,
        exc_type: type[BaseException] | None,
        exc: BaseException | None,
        tb: TracebackType | None,
    ) -> None:
        self._open = False
        self._no_report = "its stop failed"
        if self._host._recording is self:
            self._host._recording = None
        mg = self._session.microgrid_id
        path = self._session.microgrid_path("recording/stop")
        try:
            report = await self._session.post_parsed(
                path, None, RecordingReport._from_json, call=f"{self._label} stop"
            )
        except ServerError as err:
            if err.status != 404:
                raise
            self._no_report = "its microgrid was unloaded during the block"
            _log.warning(
                "Microgrid %d was unloaded during recording %r; it has no report",
                mg,
                self._name,
            )
            return
        self._report = report
        if report.ended_by == "rebuild":
            _log.warning(
                "Microgrid %d was rebuilt during recording %r, which ended at the "
                "rebuild; the rest of the block was not recorded",
                mg,
                self._name,
            )

    async def event(self, kind: str, payload: Any = None) -> int | None:
        """Add an event named `kind` to the recording, with any JSON value as
        its `payload`; returns the event's id. Once a rebuild has ended the
        recording, logs a warning, drops the event and returns ``None``; any
        other refusal raises `Rejected`."""
        check_type(f"{self._label} event kind", kind, str)
        if not self._open:
            raise RuntimeError(f"{self._label} is not open; call event inside its block")
        path = self._session.microgrid_path("recording/event")
        try:
            return await self._session.post_parsed(
                path,
                {"kind": kind, "payload": payload},
                lambda b: int(b["id"]),
                call=f"{self._label}.event({kind!r})",
            )
        except Rejected as err:
            if err._fields.get("ended_by") != "rebuild":
                raise
            _log.warning(
                "Microgrid %d dropped event %r of recording %r: %s",
                self._session.microgrid_id,
                kind,
                self._name,
                err.text,
            )
            return None

    def report(self) -> RecordingReport:
        """What the recording measured, once its block has exited. Raises
        `RuntimeError` inside the block, and after a block whose microgrid was
        unloaded."""
        if self._report is None:
            raise RuntimeError(f"{self._label} has no report: {self._no_report}")
        return self._report
