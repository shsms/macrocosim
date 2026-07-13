"""ScenarioRun tests — a fake HTTP site, no binary."""

from __future__ import annotations

from datetime import timedelta

import pytest
from frequenz.quantities import Power

import switchyard.scenarios as scenarios_mod
from switchyard.enums import Metric, Schedule
from switchyard.scenarios import Scenario, ScenarioRun, run_scenario_stepped


class _FakeProc:
    def __init__(self, returncode: int, stdout: str) -> None:
        self.returncode = returncode
        self.stdout = stdout
        self.stderr = ""


class FakeHttp:
    def __init__(self, report: dict) -> None:
        self._report = report
        self.posts: list[str] = []

    def get_json(self, path: str):
        if path == "/api/scenarios":
            return [{"name": "s", "length_s": 0.0}]
        if path == "/api/scenario":
            return {"ended_at": "2026-01-01T00:00:00Z", "elapsed_s": 1.0}
        if path == "/api/scenario/report":
            return self._report
        if path.startswith("/api/scenario/events"):
            return {"events": [{"kind": "note", "payload": "hi"}], "next_event_id": 1}
        raise AssertionError(f"unexpected GET {path}")

    def post(self, path: str, content: str = "") -> dict:
        self.posts.append(path)
        return {}


class FakeSite:
    def __init__(self, report: dict) -> None:
        self._http = FakeHttp(report)


def test_run_starts_and_stops() -> None:
    site = FakeSite({"checks_passed": 1, "checks_failed": 0, "checks": []})
    ScenarioRun(site, "s").run(wait=True)
    assert site._http.posts == ["/api/scenarios/s/start", "/api/scenarios/stop"]


def test_run_wait_without_length_raises_before_starting() -> None:
    class NoLengthHttp(FakeHttp):
        def get_json(self, path: str):
            if path == "/api/scenarios":
                return [{"name": "s"}]  # no length_s
            return super().get_json(path)

    site = FakeSite({})
    site._http = NoLengthHttp({})
    with pytest.raises(ValueError, match="no :length"):
        ScenarioRun(site, "s").run(wait=True)
    assert site._http.posts == []  # never started → nothing to orphan


def test_assert_passed_returns_report_when_clean() -> None:
    site = FakeSite({"checks_passed": 2, "checks_failed": 0, "checks": []})
    report = ScenarioRun(site, "s").assert_passed()
    assert report["checks_passed"] == 2


def test_assert_passed_raises_on_failure() -> None:
    report = {
        "checks_passed": 0,
        "checks_failed": 1,
        "checks": [{"passed": False, "detail": "too high"}],
    }
    with pytest.raises(AssertionError, match="1 check\\(s\\) failed"):
        ScenarioRun(FakeSite(report), "s").assert_passed()


def test_events_unwraps_list() -> None:
    events = ScenarioRun(FakeSite({}), "s").events()
    assert events == [{"kind": "note", "payload": "hi"}]


def test_scenario_authoring_emits_define_scenario() -> None:
    scn = (
        Scenario(
            "s",
            schedule=Schedule.RELATIVE,
            length=timedelta(minutes=4),
            seed=42,
            description="d",
        )
        .check(
            timedelta(seconds=1),
            component=2,
            metric=Metric.ACTIVE_POWER,
            approx=Power.from_watts(5000),
            tol=Power.from_watts(500),
        )
        .drive_meter(2, Power.from_megawatts(2))
        .cue(timedelta(seconds=60), '(event \'clouds "rolling in")')
    )
    lisp = scn.to_lisp()
    assert lisp.startswith(
        '(define-scenario :name "s" :description "d" :schedule \'relative'
    )
    assert ':length "240s"' in lisp
    assert ":seed 42" in lisp
    assert (
        '(check "1s" :component 2 :metric \'active-power :approx 5000.0 :tol 500.0)'
        in lisp
    )
    assert ":drive (list (drive-meter 2 2000000.0))" in lisp
    assert ':cues (list (at "60s" (event \'clouds "rolling in")))' in lisp


def test_run_scenario_stepped_returns_report(monkeypatch, tmp_path) -> None:
    monkeypatch.setattr(scenarios_mod, "_resolve_binary", lambda *a, **k: "swctl")
    monkeypatch.setattr(
        scenarios_mod.subprocess,
        "run",
        lambda *a, **k: _FakeProc(0, '{"checks_passed": 1, "checks_failed": 0}'),
    )
    report = run_scenario_stepped(str(tmp_path / "c.lisp"), "s")
    assert report["checks_passed"] == 1


def test_run_scenario_stepped_raises_on_nonzero_exit(monkeypatch, tmp_path) -> None:
    monkeypatch.setattr(scenarios_mod, "_resolve_binary", lambda *a, **k: "swctl")
    monkeypatch.setattr(
        scenarios_mod.subprocess,
        "run",
        lambda *a, **k: _FakeProc(
            1, '{"checks_failed": 1, "checks": [{"passed": false, "metric": "x"}]}'
        ),
    )
    with pytest.raises(AssertionError, match="failed"):
        run_scenario_stepped(str(tmp_path / "c.lisp"), "s")


def test_scenario_name_escaped_and_time_non_scientific() -> None:
    scn = Scenario('a"b', length=timedelta(days=30)).check(
        timedelta(seconds=1),
        component=2,
        metric=Metric.ACTIVE_POWER,
        approx=Power.from_watts(1),
        tol=Power.from_watts(1),
    )
    lisp = scn.to_lisp()
    assert '\\"' in lisp  # the embedded quote was escaped
    assert "2592000s" in lisp  # 30 days as plain integer seconds
    assert "e+" not in lisp  # never scientific notation


def test_sub_100_microsecond_offsets_render_fixed_point() -> None:
    # repr(5e-05) is scientific notation, which the server's parse-offset
    # rejects — fractional offsets must render as plain decimals.
    scn = Scenario("s", length=timedelta(seconds=1)).check(
        timedelta(microseconds=50),
        component=2,
        metric=Metric.ACTIVE_POWER,
        min=Power.from_watts(0),
    )
    lisp = scn.to_lisp()
    assert '"0.00005s"' in lisp
    assert "e-05" not in lisp
