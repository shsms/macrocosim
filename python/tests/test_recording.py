"""Microgrid.record: the context manager against a fake simulator, and live."""

from __future__ import annotations

import json
import logging
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

import httpx
import pytest

import macrocosim as mc
import macrocosim._spec as s
from macrocosim import Recording, RecordingReport, Rejected, ServerError
from macrocosim._microgrid import Microgrid
from macrocosim._process import Endpoints
from macrocosim._session import _Session

pytestmark = pytest.mark.filterwarnings("error")


def _report(ended_by: str = "stop", name: str = "peak") -> dict[str, Any]:
    return {
        "name": name,
        "elapsed_s": 12.5,
        "ended_by": ended_by,
        "peak_grid_w": 30000.0,
        "peak_grid_var": 1200.0,
        "site_pf_at_reactive_peak": 0.99,
        "total_battery_charged_wh": 10.0,
        "total_battery_discharged_wh": 2.0,
        "total_pv_produced_wh": 0.0,
        "per_battery": [{"id": 3, "charge_wh": 10.0, "discharge_wh": 2.0}],
        "per_pv": [],
        "soc_stats": {"mean_pct": 50.0, "median_pct": 50.0, "mode_pct": 50},
        "grid_window_averages": [
            {"window_start": "2026-10-06T12:00:00.000Z", "avg_w": 25000.0}
        ],
    }


class _Fake:
    """A simulator that answers the recording routes; `stop` is what the stop
    answers."""

    def __init__(self, stop: httpx.Response | None = None) -> None:
        self.stop = stop or httpx.Response(200, json=_report())
        self.start = httpx.Response(204)
        self.event = httpx.Response(200, json={"id": 4})
        self.requests: list[tuple[str, Any]] = []

    def __call__(self, request: httpx.Request) -> httpx.Response:
        body = json.loads(request.content) if request.content else None
        self.requests.append((request.url.path, body))
        route = request.url.path.removeprefix("/api/mg/7/recording/")
        if route == "start":
            return self.start
        if route == "stop":
            return self.stop
        if route == "event":
            return self.event
        return httpx.Response(404, json={"error": "no route"})

    @property
    def paths(self) -> list[str]:
        return [p.removeprefix("/api/mg/7/recording/") for p, _ in self.requests]


def _mg(fake: _Fake) -> Microgrid:
    session = _Session(
        "http://sim", 7, transport=httpx.MockTransport(fake), exit_probe=lambda: None
    )
    endpoints = Endpoints(ui="127.0.0.1:8801", microgrid_id=7, grpc_addr="x:1")
    spec = s.make_microgrid(topology=s.make_grid_connection_point(id=1))
    return Microgrid._create(session, spec, endpoints)


async def test_start_event_stop_and_report(caplog: pytest.LogCaptureFixture) -> None:
    fake = _Fake()
    mg = _mg(fake)
    recording = mg.record("peak", csv=True)
    assert isinstance(recording, Recording)
    with caplog.at_level(logging.WARNING):
        async with recording as r:
            assert r is recording
            assert await r.event("spike", {"kw": 30}) == 4
            with pytest.raises(RuntimeError, match="block has not exited"):
                r.report()
    assert not caplog.records
    assert fake.paths == ["start", "event", "stop"]
    assert fake.requests[0][1] == {"name": "peak", "csv": True}
    assert fake.requests[1][1] == {"kind": "spike", "payload": {"kw": 30}}
    report = recording.report()
    assert isinstance(report, RecordingReport)
    assert report.name == "peak" and report.ended_by == "stop"
    assert report.per_battery[0].charge_wh == 10.0
    assert report.soc_stats is not None and report.soc_stats.mode_pct == 50
    assert report.grid_window_averages[0].window_start == datetime(
        2026, 10, 6, 12, tzinfo=UTC
    )
    with pytest.raises(AttributeError):
        report.name = "other"  # type: ignore[misc]


async def test_a_rebuild_warns_and_does_not_raise(
    caplog: pytest.LogCaptureFixture,
) -> None:
    mg = _mg(_Fake(httpx.Response(200, json=_report(ended_by="rebuild"))))
    with caplog.at_level(logging.WARNING, logger="macrocosim"):
        async with mg.record("peak") as r:
            pass
    assert r.report().ended_by == "rebuild"
    [record] = caplog.records
    assert record.levelno == logging.WARNING
    assert "Microgrid 7 was rebuilt during recording 'peak'" in record.getMessage()


async def test_an_event_after_a_rebuild_warns_and_is_dropped(
    caplog: pytest.LogCaptureFixture,
) -> None:
    fake = _Fake(httpx.Response(200, json=_report(ended_by="rebuild")))
    fake.event = httpx.Response(
        409,
        json={
            "error": "the recording in microgrid 7 ended when the microgrid was rebuilt",
            "ended_by": "rebuild",
        },
    )
    mg = _mg(fake)
    with caplog.at_level(logging.WARNING, logger="macrocosim"):
        async with mg.record("peak") as r:
            assert await r.event("late", 1) is None
    first = caplog.records[0].getMessage()
    assert "Microgrid 7 dropped event 'late' of recording 'peak'" in first
    assert "ended when the microgrid was rebuilt" in first
    assert fake.paths == ["start", "event", "stop"]


async def test_an_event_refused_for_another_reason_raises() -> None:
    fake = _Fake()
    fake.event = httpx.Response(
        409, json={"error": "no recording is running in microgrid 7"}
    )
    mg = _mg(fake)
    with pytest.raises(Rejected, match="no recording is running"):
        async with mg.record("peak") as r:
            await r.event("late", 1)


async def test_an_unload_warns_and_leaves_no_report(
    caplog: pytest.LogCaptureFixture,
) -> None:
    gone = httpx.Response(404, json={"error": "microgrid 7 not registered"})
    mg = _mg(_Fake(gone))
    with caplog.at_level(logging.WARNING, logger="macrocosim"):
        async with mg.record("peak") as r:
            pass
    [record] = caplog.records
    assert "Microgrid 7 was unloaded during recording 'peak'" in record.getMessage()
    with pytest.raises(RuntimeError, match="was unloaded"):
        r.report()


async def test_the_block_stops_on_an_exception() -> None:
    fake = _Fake()
    mg = _mg(fake)
    with pytest.raises(KeyError):
        async with mg.record("peak"):
            raise KeyError("boom")
    assert fake.paths == ["start", "stop"]
    async with mg.record("again"):
        pass


async def test_one_recording_at_a_time() -> None:
    fake = _Fake()
    mg = _mg(fake)
    async with mg.record("first") as first:
        with pytest.raises(RuntimeError, match="already has a recording open"):
            mg.record("second")
    with pytest.raises(RuntimeError, match="already entered"):
        async with first:
            pass
    waiting = mg.record("a")
    async with mg.record("b"):
        with pytest.raises(RuntimeError, match="already has a recording open"):
            async with waiting:
                pass
    assert fake.paths == ["start", "stop", "start", "stop"]


async def test_a_refused_start_raises_and_frees_the_microgrid() -> None:
    fake = _Fake()
    fake.start = httpx.Response(409, json={"error": "a Lisp scenario is running"})
    mg = _mg(fake)
    with pytest.raises(Rejected, match="a Lisp scenario is running"):
        async with mg.record("peak"):
            pass
    fake.start = httpx.Response(204)
    async with mg.record("peak"):
        pass


async def test_other_stop_failures_raise() -> None:
    mg = _mg(_Fake(httpx.Response(500, json={"error": "disk full"})))
    with pytest.raises(ServerError, match="disk full"):
        async with mg.record("peak"):
            pass


async def test_bad_arguments() -> None:
    mg = _mg(_Fake())
    with pytest.raises(TypeError, match="name must be"):
        mg.record(5)  # type: ignore[arg-type]
    with pytest.raises(TypeError, match="csv must be"):
        mg.record("x", csv="yes")  # type: ignore[arg-type]
    recording = mg.record("x")
    with pytest.raises(RuntimeError, match="not open"):
        await recording.event("note")


def _live_spec() -> mc.MicrogridSpec:
    return mc.make_microgrid(
        id=7,
        topology=mc.make_grid_connection_point(
            id=1,
            successors=[
                mc.make_meter(id=2),
                mc.make_battery_inverter(id=3, successors=[mc.make_battery(id=4)]),
            ],
        ),
    )


@pytest.mark.live
async def test_live_recording(tmp_path: Path, caplog: pytest.LogCaptureFixture) -> None:
    async with mc.launch(_live_spec(), state_dir=tmp_path / "mc") as mg:
        async with mg.record("evening peak", csv=True) as r:
            assert await r.event("spike", "30 kW") == 0
        report = r.report()
        assert report.name == "evening peak" and report.ended_by == "stop"
        assert report.soc_stats is not None
        events = await mg._session.get_json(
            mg._session.microgrid_path("recording/events")
        )
        assert [e["payload"] for e in events["events"]] == ["30 kW"]
        [folder] = (tmp_path / "mc" / "recordings" / "7").iterdir()
        assert folder.name.startswith("evening_peak-")
        assert (folder / "2-meter.csv").exists()

        # A rebuild (here a PUT of the same spec, as the UI's editor sends) ends
        # the recording; leaving the block warns and does not raise.
        spec_path = mg._session.microgrid_path("spec")
        spec = await mg._session.get_json(spec_path)
        with caplog.at_level(logging.WARNING, logger="macrocosim"):
            async with mg.record("edited") as r:
                async with httpx.AsyncClient(base_url=mg.ui_url, trust_env=False) as c:
                    (await c.put(spec_path, json=spec)).raise_for_status()
                assert await r.event("late") is None
        assert r.report().ended_by == "rebuild"
        messages = [rec.getMessage() for rec in caplog.records]
        assert any("dropped event 'late'" in m for m in messages)
        assert any("was rebuilt" in m for m in messages)
