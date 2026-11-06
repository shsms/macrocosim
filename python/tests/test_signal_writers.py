"""Signals over a mocked HTTP transport: write bodies, sources and aggregates."""

from __future__ import annotations

import json
import math
from collections.abc import AsyncIterator
from datetime import UTC, datetime, timedelta
from typing import Any

import httpx
import pytest
import pytest_asyncio
from frequenz.quantities import Energy, Percentage, Power, ReactivePower
from sampling_fakes import FakeClock

from macrocosim._enums import CommandMode, Health, TelemetryMode
from macrocosim._errors import ExpectationFailed, NoSample, Rejected, ServerError
from macrocosim._matchers import at_least, at_most, between, near
from macrocosim._session import _Session
from macrocosim._signals import (
    AGGREGATES,
    COMMAND_MODE,
    HEALTH,
    POWER,
    REACTIVE,
    SOC,
    SUNLIGHT,
    TELEMETRY_MODE,
    ClearableKnob,
    ClearableSignal,
    Knob,
    SettableSignal,
    Signal,
    _AggregateSource,
    _ComponentHistorySource,
    _epoch_s,
    _Writer,
)

pytestmark = pytest.mark.filterwarnings("error")

T0 = 1_700_000_000.0


class Server:
    """Records every request; answers from a per-path handler."""

    def __init__(self, clock: FakeClock | None = None) -> None:
        self.clock = clock or FakeClock()
        self.requests: list[tuple[str, str, Any]] = []
        self.sessions: list[_Session] = []
        self.latest: dict[str, float | None] = {}
        self.history: dict[str, list[tuple[float, float | None]]] = {}
        self.ticking: dict[str, float] = {}
        """Component metrics sampled once per second of fake time."""
        self.reply_status = 204

    def handler(self, request: httpx.Request) -> httpx.Response:
        body = json.loads(request.content) if request.content else None
        self.requests.append((request.method, request.url.path, body))
        path = request.url.path
        if path.endswith("/metrics/latest"):
            now = T0 + math.floor(self.clock.t)
            ts = datetime.fromtimestamp(now, tz=UTC).isoformat().replace("+00:00", "Z")
            return httpx.Response(
                200,
                json={
                    k: {"quantity": "Power", "unit": "W", "ts": ts, "value": v}
                    for k, v in self.latest.items()
                },
            )
        if path.endswith("/metrics/history"):
            return httpx.Response(
                200,
                json={
                    k: [{"t_s": t, "value": v} for t, v in ring]
                    for k, ring in self.history.items()
                },
            )
        if path.endswith("/history"):
            metric = request.url.params["metric"]
            ring = self.history.get(metric, [])
            if metric in self.ticking:
                ring = [
                    (T0 + k, self.ticking[metric])
                    for k in range(math.floor(self.clock.t) + 1)
                ]
            return httpx.Response(200, json={"samples": [[t, v] for t, v in ring]})
        if self.reply_status >= 400:
            return httpx.Response(self.reply_status, json={"error": "refused here"})
        return httpx.Response(self.reply_status)

    def session(self, handler: Any = None) -> _Session:
        session = _Session(
            "http://sim",
            7,
            transport=httpx.MockTransport(handler or self.handler),
            exit_probe=lambda: None,
        )
        self.sessions.append(session)
        return session


@pytest_asyncio.fixture
async def srv() -> AsyncIterator[Server]:
    server = Server()
    yield server
    for session in server.sessions:
        await session.close()


def _writer(session: _Session, label: str, definition: Any) -> _Writer:
    return _Writer(session, 104, label, definition.kind, definition.setter)


async def test_drive_bodies_for_every_writer(srv: Server) -> None:
    session = srv.session()
    clock = srv.clock

    def hist(metric: str, kind: type) -> _ComponentHistorySource[Any]:
        return _ComponentHistorySource(session, 104, metric, kind, "x")

    power = ClearableSignal(
        hist("active_power_w", Power),
        _writer(session, "Meter 104 power", POWER),
        clock=clock,
    )
    reactive = ClearableSignal(
        hist("reactive_power_var", ReactivePower),
        _writer(session, "Meter 104 reactive_power", REACTIVE),
        clock=clock,
    )
    soc = SettableSignal(
        hist("soc_pct", Percentage), _writer(session, "Battery 104 soc", SOC), clock=clock
    )
    sunlight = ClearableKnob(_writer(session, "SolarInverter 104 sunlight", SUNLIGHT))

    await power.set(Power.from_kilowatts(20))
    await power.clear()
    await reactive.set(ReactivePower.from_volt_amperes_reactive(1500))
    await reactive.clear()
    await soc.set(Percentage.from_percent(55))
    await sunlight.set(Percentage.from_percent(30))
    await sunlight.clear()
    drive = "/api/mg/7/component/104/drive"
    assert srv.requests == [
        ("POST", drive, {"power_w": 20000.0}),
        ("POST", drive, {"clear_power": True}),
        ("POST", drive, {"reactive_power_var": 1500.0}),
        ("POST", drive, {"clear_reactive": True}),
        ("POST", drive, {"soc_pct": 55.0}),
        ("POST", drive, {"sunlight_pct": 30.0}),
        ("POST", drive, {"clear_sunlight": True}),
    ]


async def test_status_posts_one_typed_field_per_request(srv: Server) -> None:
    session = srv.session()
    health = Knob(_writer(session, "Meter 104 health", HEALTH))
    command = Knob(_writer(session, "Meter 104 command_mode", COMMAND_MODE))
    telemetry = Knob(_writer(session, "Meter 104 telemetry_mode", TELEMETRY_MODE))
    await health.set(Health.ERROR)
    await command.set(CommandMode.TIMEOUT)
    await telemetry.set(TelemetryMode.SILENT)
    status = "/api/mg/7/component/104/status"
    assert srv.requests == [
        ("POST", status, {"health": "error"}),
        ("POST", status, {"command_mode": "timeout"}),
        ("POST", status, {"telemetry_mode": "silent"}),
    ]


async def test_drive_refuses_a_bare_number_or_a_non_finite_value(srv: Server) -> None:
    session = srv.session()
    power = ClearableSignal(
        _ComponentHistorySource(session, 104, "active_power_w", Power, "x"),
        _writer(session, "Meter 104 power", POWER),
    )
    sunlight = ClearableKnob(_writer(session, "SolarInverter 104 sunlight", SUNLIGHT))
    with pytest.raises(TypeError, match="power must be Power, not float"):
        await power.set(2000.0)  # type: ignore[arg-type]
    with pytest.raises(TypeError, match="sunlight must be Percentage, not float"):
        await sunlight.set(30.0)  # type: ignore[arg-type]
    with pytest.raises(ValueError, match="power must be finite"):
        await power.set(Power.from_watts(float("nan")))
    with pytest.raises(TypeError, match="health must be Health"):
        await Knob(_writer(session, "Meter 104 health", HEALTH)).set("error")
    assert srv.requests == []


async def test_a_refused_write_names_the_call(srv: Server) -> None:
    srv.reply_status = 400
    session = srv.session()
    soc = SettableSignal(
        _ComponentHistorySource(session, 104, "soc_pct", Percentage, "x"),
        _writer(session, "Battery 104 soc", SOC),
    )
    with pytest.raises(Rejected) as info:
        await soc.set(Percentage.from_percent(120))
    assert str(info.value) == "Battery 104 soc.set(120 %) was rejected: refused here"


async def test_meter_reactive_power_reads_its_history_metric(srv: Server) -> None:
    session = srv.session()
    clock = srv.clock
    srv.ticking = {"reactive_power_var": 750.0, "active_power_w": 99.0}
    reactive = Signal(
        _ComponentHistorySource(session, 104, "reactive_power_var", ReactivePower, "x"),
        clock=clock,
    )
    value = await reactive.read(timeout=timedelta(seconds=2))
    assert value == ReactivePower.from_volt_amperes_reactive(750)
    assert {p for _, p, _ in srv.requests} == {"/api/mg/7/component/104/history"}


def test_ts_is_parsed_from_rfc_3339() -> None:
    assert (
        _epoch_s("2026-10-06T12:00:00.000Z")
        == datetime(2026, 10, 6, 12, tzinfo=UTC).timestamp()
    )
    assert _epoch_s("2026-10-06T12:00:00.250Z") % 1 == pytest.approx(0.25)


def _aggregate(srv: Server, name: str) -> Signal[Any]:
    session = srv.session()
    [definition] = [a for a in AGGREGATES if a.name == name]
    return Signal(_AggregateSource(session, definition), clock=srv.clock)


async def test_battery_aggregates_read_the_pool_streams(srv: Server) -> None:
    srv.latest = {"battery_pool_power": -2000.0, "battery_pool_energy": -50.0}
    await _aggregate(srv, "battery_power").expect(
        near(Power.from_watts(-2000), tol=Power.from_watts(1))
    )
    await _aggregate(srv, "battery_energy").expect(
        near(Energy.from_watt_hours(-50), tol=Energy.from_watt_hours(1))
    )


async def test_an_aggregate_without_a_sample_times_out(srv: Server) -> None:
    with pytest.raises(NoSample, match="no new sample with a value within 2 s"):
        await _aggregate(srv, "grid_power").read(timeout=timedelta(seconds=2))


kWh = Energy.from_kilowatt_hours


async def test_expect_grid_energy_passes_within_matcher(srv: Server) -> None:
    srv.latest = {"grid_energy": 1500.0}
    await _aggregate(srv, "grid_energy").expect(between(kWh(1), kWh(2)))


async def test_expect_grid_energy_raises_when_over_cap(srv: Server) -> None:
    srv.latest = {"grid_energy": 2500.0}
    with pytest.raises(ExpectationFailed, match="never matched <= 2 kWh"):
        await _aggregate(srv, "grid_energy").expect(at_most(kWh(2)))


async def test_expect_consumer_energy_reads_its_own_stream(srv: Server) -> None:
    srv.latest = {"grid_energy": 9000.0, "consumer_energy": 1200.0}
    await _aggregate(srv, "consumer_energy").expect(at_most(kWh(2)))


async def test_expect_pv_energy_within_matcher_negative(srv: Server) -> None:
    srv.latest = {"pv_energy": -800.0}
    await _aggregate(srv, "pv_energy").expect(between(kWh(-1), kWh(0)))


async def test_expect_battery_energy_approx_matcher(srv: Server) -> None:
    srv.latest = {"battery_pool_energy": 400.0}
    await _aggregate(srv, "battery_energy").expect(
        near(kWh(0.4), tol=Energy.from_watt_hours(10))
    )


async def test_expect_battery_energy_raises_under_floor(srv: Server) -> None:
    srv.latest = {"battery_pool_energy": -400.0}
    with pytest.raises(ExpectationFailed, match="never matched >= 0 Wh"):
        await _aggregate(srv, "battery_energy").expect(
            at_least(Energy.from_watt_hours(0))
        )


async def test_an_aggregate_hold_reads_its_stream_from_history(srv: Server) -> None:
    srv.latest = {"grid_power": 100.0}
    srv.history = {
        "grid_power": [(T0 + k, 100.0) for k in range(0, 40)],
        "pv_power": [(T0 + k, 99999.0) for k in range(0, 40)],
    }
    await _aggregate(srv, "grid_power").expect(
        at_most(Power.from_watts(200)), hold_for=timedelta(seconds=5)
    )
    assert any(p.endswith("/metrics/history") for _, p, _ in srv.requests)


async def test_an_answer_of_the_wrong_shape_is_a_server_error(srv: Server) -> None:
    def wrong(request: httpx.Request) -> httpx.Response:
        if request.url.path.endswith("/metrics/history"):
            return httpx.Response(200, json=[1, 2])
        if request.url.path.endswith("/metrics/latest"):
            return httpx.Response(200, json={"grid_power": [1]})
        return httpx.Response(200, json={"unexpected": []})

    session = srv.session(wrong)
    history = Signal(_ComponentHistorySource(session, 104, "soc_pct", Percentage, "x"))
    with pytest.raises(ServerError, match="did not expect"):
        await history.read(timeout=timedelta(seconds=1))
    [definition] = [a for a in AGGREGATES if a.name == "grid_power"]
    source = _AggregateSource(session, definition)
    with pytest.raises(ServerError, match="did not expect"):
        await source.fetch_hold(10)
    with pytest.raises(ServerError, match="did not expect"):
        await source.fetch(3)
