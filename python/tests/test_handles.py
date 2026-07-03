"""Handle-layer tests — a fake site records what command/fault/drive emit."""

from __future__ import annotations

from datetime import timedelta

from frequenz.quantities import Percentage, Power

from switchyard.build import raw
from switchyard.enums import CommandMode, Health, TelemetryMode
from switchyard.handles import ComponentHandle


class FakeSite:
    def __init__(self) -> None:
        self.evals: list[str] = []
        self.setpoints: list[tuple[int, float, float | None]] = []
        self.bounds: list[tuple[int, float, float]] = []

    def eval(self, expr: str, mg_id: int | None = None) -> None:
        self.evals.append(expr)

    def set_active_power(
        self, cid: int, power: Power, *, lifetime: timedelta | None = None, mg_id=None
    ) -> None:
        secs = None if lifetime is None else lifetime.total_seconds()
        self.setpoints.append((cid, power.as_watts(), secs))

    def augment_bounds(
        self, cid: int, lower: Power, upper: Power, mg_id: int | None = None
    ) -> None:
        self.bounds.append((cid, lower.as_watts(), upper.as_watts()))


def _h(site: FakeSite, cid: int = 3) -> ComponentHandle:
    return ComponentHandle(site, cid)


def test_status_emits_symbols() -> None:
    w = FakeSite()
    _h(w).status(health=Health.ERROR)
    _h(w).status(command_mode=CommandMode.TIMEOUT)
    _h(w).status(telemetry_mode=TelemetryMode.SILENT)
    assert w.evals == [
        "(set-component-health 3 'error)",
        "(set-component-command-mode 3 'timeout)",
        "(set-component-telemetry-mode 3 'silent)",
    ]


def test_command_setpoint_and_bounds_go_grpc() -> None:
    w = FakeSite()
    _h(w).command(active_power=Power.from_kilowatts(2), lifetime=timedelta(seconds=30))
    _h(w).command(bounds=(Power.from_kilowatts(-1), Power.from_kilowatts(1)))
    assert w.setpoints == [(3, 2000.0, 30.0)]
    assert w.bounds == [(3, -1000.0, 1000.0)]
    assert w.evals == []  # gateway commands don't touch eval


def test_drive_power_units_and_raw() -> None:
    w = FakeSite()
    _h(w, 6).drive(power=Power.from_megawatts(2))
    _h(w, 6).drive(power=Power.from_watts(-5000))
    _h(w, 6).drive(power=raw("(lambda () (+ 1000.0 (random 500)))"))
    assert w.evals == [
        "(set-meter-power 6 2000000.0)",
        "(set-meter-power 6 -5000.0)",
        "(set-meter-power 6 (lambda () (+ 1000.0 (random 500))))",
    ]


def test_drive_sunlight() -> None:
    w = FakeSite()
    _h(w, 5).drive(sunlight=Percentage.from_percent(30))
    assert w.evals == ["(set-solar-sunlight 5 30.0)"]


def test_handle_methods_chain() -> None:
    w = FakeSite()
    h = ComponentHandle(w, 3)
    assert h.status(health=Health.OK).command(active_power=Power.from_watts(0)) is h
