"""The polling engine, against a fake 1 Hz sampler and a fake clock."""

from __future__ import annotations

from datetime import timedelta
from pathlib import Path

import pytest
from frequenz.quantities import Power
from sampling_fakes import FakeClock, FakeSource

from macrocosim._errors import ExpectationFailed, NoSample
from macrocosim._matchers import at_most, between, near
from macrocosim._sampling import expect, read

pytestmark = pytest.mark.filterwarnings("error")

W = Power.from_watts
s_ = timedelta(seconds=1)
NO_LOG = "\n  Pass state_dir to launch to keep the simulator log."
LOG = Path("/tmp/state/macrocosim.log")


def _setup(values, **kw) -> tuple[FakeClock, FakeSource]:
    clock = FakeClock()
    return clock, FakeSource(clock, values, **kw)


# --- read ---------------------------------------------------------------------


async def test_read_returns_the_first_new_sample_as_a_quantity() -> None:
    clock, src = _setup([1500.0, 1600.0], old=[999.0])
    value = await read(src, 5 * s_, clock=clock)
    assert value == W(1500)


async def test_read_skips_nulls() -> None:
    clock, src = _setup([None, 1600.0])
    assert await read(src, 5 * s_, clock=clock) == W(1600)


async def test_read_timeout_text() -> None:
    clock, src = _setup([], old=[100.0])
    with pytest.raises(NoSample) as info:
        await read(src, 2 * s_, clock=clock)
    # The old sample has left the 3 s fetch window by then, so no samples.
    assert str(info.value) == (
        "BatteryInverter 102 power: no new sample with a value within 2 s." + NO_LOG
    )
    assert clock.t == pytest.approx(2.0)


async def test_read_ignores_staleness() -> None:
    clock, src = _setup([], old=[100.0], aggregate=True, label="microgrid.grid_power")
    with pytest.raises(NoSample, match="no new sample with a value within 20 s"):
        await read(src, 20 * s_, clock=clock)
    assert clock.t == pytest.approx(20.0)


# --- settle -------------------------------------------------------------------


async def test_baseline_excludes_old_samples() -> None:
    clock, src = _setup([900.0, 900.0, 900.0], old=[100.0, 100.0])
    with pytest.raises(ExpectationFailed, match="never matched"):
        await expect(src, at_most(W(200)), 3 * s_, clock=clock)


async def test_an_empty_first_fetch_makes_every_sample_new() -> None:
    clock, src = _setup([100.0])
    await expect(src, at_most(W(200)), 3 * s_, clock=clock)


async def test_eventually_passes_when_predicate_holds() -> None:
    clock, src = _setup([2000.0])
    await expect(src, between(W(1900), W(2100)), 3 * s_, clock=clock)


async def test_instantaneous_metric_settles() -> None:
    clock, src = _setup([900.0, 900.0, 100.0])
    await expect(src, at_most(W(200)), 5 * s_, clock=clock)


async def test_eventually_raises_when_never_holds() -> None:
    clock, src = _setup([500.0] * 5)
    with pytest.raises(ExpectationFailed, match="≈"):
        await expect(src, near(W(100), tol=W(10)), 3 * s_, clock=clock)


async def test_settle_miss_text_with_a_kept_log() -> None:
    clock, src = _setup([900.0] * 4, log_path=LOG)
    with pytest.raises(ExpectationFailed) as info:
        await expect(src, at_most(W(200)), 3 * s_, clock=clock)
    assert str(info.value) == (
        "BatteryInverter 102 power never matched <= 200 W within 3 s."
        "\n  Last 3 samples (newest last): 900 W, 900 W, 900 W"
        f"\n  Simulator log: {LOG}"
    )
    assert info.value.log_path == LOG


async def test_none_reads_never_satisfy() -> None:
    clock, src = _setup([None, None, None])
    with pytest.raises(NoSample) as info:
        await expect(src, at_most(W(200)), 2 * s_, clock=clock)
    assert str(info.value) == (
        "BatteryInverter 102 power: no new sample with a value within 2 s while "
        "waiting for <= 200 W.\n  Last 2 samples (newest last): null, null" + NO_LOG
    )


async def test_no_new_sample_during_settle_raises_no_sample() -> None:
    clock, src = _setup([], old=[100.0])
    with pytest.raises(NoSample, match="no new sample with a value within 2 s"):
        await expect(src, at_most(W(200)), 2 * s_, clock=clock)


async def test_settle_raises_at_once_when_an_aggregate_goes_stale() -> None:
    clock, src = _setup([900.0], aggregate=True, label="microgrid.grid_power")
    with pytest.raises(NoSample) as info:
        await expect(src, at_most(W(200)), 60 * s_, clock=clock)
    assert str(info.value) == (
        "microgrid.grid_power stopped producing samples while waiting for <= 200 W:"
        " no new sample for 3 s (last: 900 W)." + NO_LOG
    )
    assert clock.t < 5.0  # the sample was seen at 1 s; 3 s later it is stale


async def test_a_component_source_never_goes_stale() -> None:
    clock, src = _setup([900.0])
    with pytest.raises(ExpectationFailed, match="never matched"):
        await expect(src, at_most(W(200)), 30 * s_, clock=clock)
    assert clock.t == pytest.approx(30.0)


async def test_server_timestamps_far_behind_the_test_clock() -> None:
    clock, src = _setup([100.0] * 15, server_t0=1_700_000_000.0 - 900.0)
    await expect(src, at_most(W(200)), 5 * s_, hold_for=5 * s_, clock=clock)


async def test_failures_carry_the_signal_name() -> None:
    clock, src = _setup([900.0] * 3, aggregate=True, label="microgrid.grid_power")
    with pytest.raises(ExpectationFailed) as info:
        await expect(src, at_most(W(200)), 2 * s_, clock=clock)
    assert info.value.signal == "microgrid.grid_power"
    assert str(info.value).startswith("microgrid.grid_power never matched")


# --- hold ---------------------------------------------------------------------


async def test_always_holds_then_breaks() -> None:
    clock, src = _setup([100.0] * 15)
    await expect(src, at_most(W(200)), 5 * s_, hold_for=5 * s_, clock=clock)
    clock, src = _setup([100.0, 100.0, 999.0] + [100.0] * 10)
    with pytest.raises(ExpectationFailed, match="then held for 1 s of the 5 s"):
        await expect(src, at_most(W(200)), 5 * s_, hold_for=5 * s_, clock=clock)


async def test_always_accepts_approx_and_tol() -> None:
    clock, src = _setup([100.0] * 15)
    await expect(src, near(W(100), tol=W(10)), 5 * s_, hold_for=5 * s_, clock=clock)
    clock, src = _setup([500.0] * 15)
    with pytest.raises(ExpectationFailed, match="never matched"):
        await expect(src, near(W(100), tol=W(10)), 5 * s_, hold_for=5 * s_, clock=clock)


async def test_first_match_starts_the_hold_and_a_later_break_fails() -> None:
    # 0 W matches while the value is still moving; it does not re-arm.
    clock, src = _setup([0.0, 100.0, 0.0, -4200.0, -4300.0] + [0.0] * 10)
    with pytest.raises(ExpectationFailed) as info:
        await expect(src, near(W(0), tol=W(1000)), 10 * s_, hold_for=10 * s_, clock=clock)
    assert str(info.value) == (
        "BatteryInverter 102 power matched ≈ 0 W ± 1 kW, then held for 2 s of the "
        "10 s asked for (hold_for).\n  At T0+3 s it read -4.2 kW."
        "\n  Last 4 samples (newest last): 0 W, 100 W, 0 W, -4.2 kW" + NO_LOG
    )


async def test_a_gap_over_the_limit_raises_no_sample() -> None:
    clock, src = _setup([100.0] * 15, skip=[4, 5])
    with pytest.raises(NoSample) as info:
        await expect(src, at_most(W(200)), 10 * s_, hold_for=10 * s_, clock=clock)
    assert str(info.value) == (
        "BatteryInverter 102 power has a 3 s gap in its samples at T0+3 s of a "
        "10 s hold (limit 2.5 s)."
        "\n  Last 4 samples (newest last): 100 W, 100 W, 100 W, 100 W" + NO_LOG
    )


async def test_one_missed_tick_passes() -> None:
    clock, src = _setup([100.0] * 15, skip=[4])
    await expect(src, at_most(W(200)), 10 * s_, hold_for=10 * s_, clock=clock)


async def test_a_null_in_the_hold_raises_no_sample() -> None:
    clock, src = _setup([100.0, 100.0, None] + [100.0] * 12)
    with pytest.raises(NoSample) as info:
        await expect(src, at_most(W(200)), 10 * s_, hold_for=10 * s_, clock=clock)
    assert str(info.value) == (
        "BatteryInverter 102 power had no value at T0+2 s of a 10 s hold."
        "\n  Last 3 samples (newest last): 100 W, 100 W, null" + NO_LOG
    )


async def test_samples_stopping_in_the_hold_raise_no_sample() -> None:
    clock, src = _setup([100.0] * 5, log_path=LOG)
    with pytest.raises(NoSample) as info:
        await expect(src, at_most(W(200)), 10 * s_, hold_for=10 * s_, clock=clock)
    assert str(info.value) == (
        "BatteryInverter 102 power stopped producing samples at T0+4 s of a 10 s "
        f"hold (last: 100 W).\n  Simulator log: {LOG}"
    )


async def test_an_empty_hold_window_raises_no_sample() -> None:
    clock, src = _setup([100.0] * 15, hold_override=[])
    with pytest.raises(NoSample) as info:
        await expect(src, at_most(W(200)), 10 * s_, hold_for=10 * s_, clock=clock)
    assert str(info.value) == (
        "BatteryInverter 102 power has no samples in its 10 s hold window after T0."
        + NO_LOG
    )


async def test_the_hold_window_is_measured_from_t0() -> None:
    clock, src = _setup([100.0] * 30)
    await expect(src, at_most(W(200)), 10 * s_, hold_for=10 * s_, clock=clock)
    # T0 was seen at 1 s; the hold fetch comes 10 s later: ceil(10) + 3.
    assert src.hold_windows == [13]


async def test_an_aggregate_hold_reads_the_whole_ring() -> None:
    clock, src = _setup([100.0] * 15, aggregate=True, label="microgrid.grid_power")
    await expect(src, at_most(W(200)), 5 * s_, hold_for=5 * s_, clock=clock)


# --- argument checks ----------------------------------------------------------


async def test_timeout_and_hold_for_checks() -> None:
    clock, src = _setup([100.0])
    m = at_most(W(200))
    with pytest.raises(TypeError, match="timeout must be timedelta, not"):
        await expect(src, m, 5.0, clock=clock)  # type: ignore[arg-type]
    with pytest.raises(ValueError, match="timeout must be positive"):
        await expect(src, m, timedelta(0), clock=clock)
    with pytest.raises(TypeError, match="hold_for must be timedelta, not int"):
        await expect(src, m, s_, 5, clock=clock)  # type: ignore[arg-type]
    with pytest.raises(ValueError, match="between 1 s and 540 s"):
        await expect(src, m, s_, timedelta(milliseconds=500), clock=clock)
    with pytest.raises(ValueError, match="not 541 s"):
        await expect(src, m, s_, timedelta(seconds=541), clock=clock)
    with pytest.raises(TypeError, match="timeout must be timedelta, not"):
        await read(src, 5, clock=clock)  # type: ignore[arg-type]
    assert clock.t == 0.0  # no I/O, no waiting


async def test_an_aggregate_allows_a_longer_hold() -> None:
    clock, src = _setup([100.0], aggregate=True, label="microgrid.grid_power")
    with pytest.raises(ValueError, match="between 1 s and 900 s"):
        await expect(src, at_most(W(200)), s_, timedelta(seconds=901), clock=clock)


async def test_read_returns_the_oldest_of_several_new_samples() -> None:
    class TwoAtOnce(FakeSource):
        calls = 0

        async def fetch(self, window_s: int) -> list:
            self.calls += 1
            return [] if self.calls == 1 else [(2.0, 200.0), (1.0, 100.0)]

    clock = FakeClock()
    assert await read(TwoAtOnce(clock, []), 5 * s_, clock=clock) == W(100)


async def test_an_aggregate_with_only_an_old_sample_is_not_stale() -> None:
    clock, src = _setup([], old=[100.0], aggregate=True, label="microgrid.grid_power")
    with pytest.raises(NoSample, match="no new sample with a value within 20 s while"):
        await expect(src, at_most(W(200)), 20 * s_, clock=clock)
    assert clock.t == pytest.approx(20.0)


async def test_a_sample_at_the_end_of_the_hold_is_checked() -> None:
    clock, src = _setup([100.0] * 5 + [999.0] + [100.0] * 10)
    with pytest.raises(ExpectationFailed, match="then held for 4 s of the 5 s"):
        await expect(src, at_most(W(200)), 5 * s_, hold_for=5 * s_, clock=clock)


async def test_a_gap_at_the_end_of_the_hold_raises_no_sample() -> None:
    clock, src = _setup([100.0] * 15, skip=[5, 6, 7])
    with pytest.raises(NoSample, match="has a 3 s gap in its samples at T0\\+4 s"):
        await expect(src, at_most(W(200)), 5 * s_, hold_for=7 * s_, clock=clock)


async def test_the_longest_hold_is_accepted() -> None:
    clock, src = _setup([100.0] * 560)
    await expect(src, at_most(W(200)), 5 * s_, hold_for=540 * s_, clock=clock)


async def test_a_duplicate_timestamp_with_a_null_raises_no_sample() -> None:
    t0 = 1_700_000_000.0
    held = [(t0 + k, 100.0) for k in range(6)] + [(t0 + 1, None)]
    clock, src = _setup([100.0] * 15, hold_override=held)
    with pytest.raises(NoSample, match="had no value at T0\\+1 s"):
        await expect(src, at_most(W(200)), 5 * s_, hold_for=5 * s_, clock=clock)
