"""Signals over a mocked HTTP transport: write bodies, sources and aggregates."""

from __future__ import annotations

import json
import math
from collections.abc import AsyncIterator
from datetime import UTC, datetime, time, timedelta
from typing import Any

import httpx
import pytest
import pytest_asyncio
from frequenz.quantities import (
    Current,
    Energy,
    Frequency,
    Percentage,
    Power,
    ReactivePower,
    Voltage,
)
from sampling_fakes import FakeClock

import macrocosim._spec as spec
from macrocosim._components import Battery, EvCharger
from macrocosim._enums import CommandMode, Health, OperationalMode, TelemetryMode
from macrocosim._errors import ExpectationFailed, NoSample, Rejected, ServerError
from macrocosim._matchers import at_least, at_most, between, near
from macrocosim._microgrid import Microgrid
from macrocosim._process import Endpoints
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


pct = Percentage.from_percent
sec = timedelta(seconds=1)


async def test_component_knobs_post_their_routes(srv: Server) -> None:
    session = srv.session()
    charger = EvCharger(session, 6)
    await Battery(session, 4).operational_mode.set(OperationalMode.TELEMETRY_ONLY)
    await charger.plug(
        "city",
        soc=pct(25),
        target_soc=pct(80),
        phases=1,
        max_current=Current.from_amperes(16),
        capacity=Energy.from_kilowatt_hours(40),
        taper_start=pct(85),
        taper_floor=pct(30),
    )
    await charger.plug("van")
    await charger.unplug()
    ev = "/api/mg/7/component/6/ev"
    assert srv.requests == [
        (
            "POST",
            "/api/mg/7/component/4/runtime",
            {"operational_mode": "telemetry_only"},
        ),
        (
            "POST",
            ev,
            {
                "preset": "city",
                "soc_pct": 25.0,
                "target_soc_pct": 80.0,
                "phases": 1,
                "max_current_a": 16.0,
                "capacity_wh": 40000.0,
                "taper_start_pct": 85.0,
                "taper_floor_pct": 30.0,
            },
        ),
        ("POST", ev, {"preset": "van"}),
        ("DELETE", ev, None),
    ]


def _microgrid(session: _Session) -> Microgrid:
    endpoints = Endpoints(ui="127.0.0.1:8801", microgrid_id=7, grpc_addr="x:1")
    topology = spec.make_microgrid(topology=spec.make_grid_connection_point(id=1))
    return Microgrid._create(session, topology, endpoints)


async def test_microgrid_knobs_post_their_routes(srv: Server) -> None:
    mg = _microgrid(srv.session())
    volts = Voltage.from_volts
    hz = Frequency.from_hertz
    await mg.set_voltage_per_phase(volts(231), volts(230), volts(229))
    await mg.set_sample_lag(timedelta(milliseconds=1500))
    await mg.cancel_streams()
    await mg.frequency.set(hz(49.8))
    await mg.frequency.clear()
    await mg.set_frequency_model(nominal=hz(50), sigma=0.02)
    await mg.set_weather(
        sunrise=time(6, 30),
        sunset=time(19),
        peak=pct(90),
        cloud_mean_gap=timedelta(minutes=10),
        cloud_depth=(pct(20), pct(60)),
        cloud_duration=(60 * sec, 600 * sec),
        cloud_ramp=(10 * sec, 30 * sec),
        seed=7,
    )
    await mg.pass_cloud(pct(50), 120 * sec)
    await mg.pass_cloud(pct(50), 120 * sec, 5 * sec)
    await mg.clear_weather()
    weather = "/api/mg/7/weather"
    assert srv.requests == [
        ("POST", "/api/mg/7/grid", {"voltage_per_phase_v": [231.0, 230.0, 229.0]}),
        ("POST", "/api/mg/7/sample-lag", {"sample_lag_s": 1.5}),
        ("POST", "/api/mg/7/streams/cancel", None),
        (
            "POST",
            "/api/frequency",
            {"hz": 49.8, "override": {"nominal_hz": 49.8, "sigma_hz_per_sqrt_s": 0}},
        ),
        ("POST", "/api/frequency", {"clear_override": True}),
        (
            "POST",
            "/api/frequency",
            {"model": {"nominal_hz": 50.0, "sigma_hz_per_sqrt_s": 0.02}},
        ),
        (
            "POST",
            weather,
            {
                "peak_pct": 90.0,
                "cloud_mean_gap_s": 600.0,
                "seed": 7,
                "sunrise": "06:30",
                "sunset": "19:00",
                "cloud_depth_pct": [20.0, 60.0],
                "cloud_duration_s": [60.0, 600.0],
                "cloud_ramp_s": [10.0, 30.0],
            },
        ),
        ("POST", weather, {"pass_cloud": {"depth_pct": 50.0, "duration_s": 120.0}}),
        (
            "POST",
            weather,
            {"pass_cloud": {"depth_pct": 50.0, "duration_s": 120.0, "ramp_s": 5.0}},
        ),
        ("DELETE", weather, None),
    ]
    assert repr(mg.frequency) == ("<ClearableKnob Microgrid 7 frequency>")


async def test_microgrid_knobs_check_their_arguments(srv: Server) -> None:
    mg = _microgrid(srv.session())
    with pytest.raises(TypeError, match="frequency must be Frequency"):
        await mg.frequency.set(50.0)  # type: ignore[arg-type]
    with pytest.raises(TypeError, match="set_sample_lag must be timedelta"):
        await mg.set_sample_lag(1.5)  # type: ignore[arg-type]
    with pytest.raises(ValueError, match="sunrise must be a whole minute"):
        await mg.set_weather(sunrise=time(6, 30, 15))
    with pytest.raises(ValueError, match="cloud_depth must be a \\(low, high\\) pair"):
        await mg.set_weather(cloud_depth=(pct(1),))  # type: ignore[arg-type]
    with pytest.raises(TypeError, match="cloud_ramp must be timedelta"):
        await mg.set_weather(cloud_ramp=(1, 2))  # type: ignore[arg-type]
    for seed in (-1, 2**64):
        with pytest.raises(ValueError, match="seed must be 0 to 2\\*\\*64 - 1"):
            await mg.set_weather(seed=seed)
    assert srv.requests == []


async def test_a_refused_microgrid_write_names_the_call(srv: Server) -> None:
    srv.reply_status = 409
    mg = _microgrid(srv.session())
    with pytest.raises(Rejected) as info:
        await mg.pass_cloud(pct(50), 60 * sec)
    assert str(info.value) == ("Microgrid 7 pass_cloud(50 %) was rejected: refused here")
