"""The polling engine behind every `read` and `expect`.

Internal; imports `_errors`, `_matchers` and `_units`, and does no I/O of its
own: a `_Source` fetches ``(t_s, value)`` pairs, where ``t_s`` is the
simulator's timestamp in epoch seconds and ``value`` a float in the wire unit,
or ``None``.

Two clocks, never compared: windows are judged only in simulator time (``t_s``),
and waiting is bounded only by the test process's monotonic clock. So a
``FAKETIME`` offset on the simulator cannot shift a window.
"""

from __future__ import annotations

import asyncio
import math
import time
from collections.abc import Awaitable, Callable, Sequence
from datetime import timedelta
from pathlib import Path
from typing import Protocol, TypeVar

from ._errors import (
    ExpectationFailed,
    NoSample,
    fmt_at,
    fmt_duration,
    fmt_last,
    fmt_log,
    fmt_value,
)
from ._matchers import Matcher
from ._units import check_type

Q = TypeVar("Q")
Q_co = TypeVar("Q_co", covariant=True)

Sample = tuple[float, "float | None"]
"""``(t_s, value)``: simulator time in epoch seconds, and the wire value."""

POLL_S = 0.5
"""Half the 1 Hz sampler period, so a stored sample is seen within 0.5 s."""

WINDOW_S = 3
"""The baseline and settle fetch window: the poll interval plus one missed
tick."""

GAP_LIMIT_S = 2.5
"""2.5 sampler periods: one missed tick (a 2 s gap) passes, two (3 s) fail."""

EXTRA_POLL_S = 5.0
"""How long the hold keeps polling for a delayed sampler to reach
``T0 + hold_for``."""

HOLD_MARGIN_S = 3
"""Added to the hold window to cover the delay between ``T0``'s stamp and
the moment the engine saw it."""

HOLD_MIN = timedelta(seconds=1)
"""One sampler period."""

HOLD_MAX_COMPONENT = timedelta(seconds=540)
"""A component ring holds 600 samples at 1 Hz; 60 s is left for the margin and
the extra poll."""

HOLD_MAX_AGGREGATE = timedelta(seconds=900)
"""An aggregate ring holds 1000 samples at 1 Hz; 100 s is left for the same."""


class _Source(Protocol[Q_co]):
    """Where one signal's samples come from."""

    from_wire: Callable[[float], Q_co]
    """Turns a wire float into the signal's quantity."""

    stale_after: float | None
    """Seconds without a new ``t_s`` after which settling gives up; ``None``
    turns that check off. Only aggregate sources use it."""

    hold_max: timedelta
    """The longest `hold_for` the source's history can cover."""

    log_path: Path | None
    """The kept simulator log, for messages."""

    async def fetch(self, window_s: int) -> Sequence[Sample]:
        """The newest samples: those of the last `window_s` seconds, or only the
        newest one for an aggregate."""
        ...

    async def fetch_hold(self, window_s: int) -> Sequence[Sample]:
        """At least the samples of the last `window_s` seconds, to check a hold
        window."""
        ...

    def describe(self) -> str:
        """The signal as messages name it (``microgrid.grid_power``)."""
        ...


class _Clock:
    """The monotonic clock and sleep the engine uses; replaced in tests."""

    def __init__(
        self,
        now: Callable[[], float] = time.monotonic,
        sleep: Callable[[float], Awaitable[object]] = asyncio.sleep,
    ) -> None:
        self.now = now
        self.sleep = sleep


REAL_CLOCK = _Clock()


def _timeout_seconds(timeout: timedelta) -> float:
    check_type("timeout", timeout, timedelta)
    seconds = timeout.total_seconds()
    if seconds <= 0:
        raise ValueError(f"timeout must be positive, not {timeout}")
    return seconds


def _check_hold(hold_for: timedelta | None, limit: timedelta) -> None:
    if hold_for is None:
        return
    check_type("hold_for", hold_for, timedelta)
    if not HOLD_MIN <= hold_for <= limit:
        raise ValueError(
            f"hold_for must be between {fmt_duration(HOLD_MIN)} and "
            f"{fmt_duration(limit)} (the history the simulator keeps), "
            f"not {fmt_duration(hold_for)}"
        )


def _sample_time(sample: Sample) -> float:
    return sample[0]


def _newest(samples: Sequence[Sample]) -> float:
    return max((t for t, _ in samples), default=-math.inf)


def _values(source: _Source[Q], samples: Sequence[Sample]) -> list[Q | None]:
    return [None if v is None else source.from_wire(v) for _, v in samples]


async def read(
    source: _Source[Q], timeout: timedelta, *, clock: _Clock = REAL_CLOCK
) -> Q:
    """The value of the first sample newer than the newest one the source had
    when the read began; samples with no value are skipped."""
    seconds = _timeout_seconds(timeout)
    samples = await source.fetch(WINDOW_S)
    baseline = _newest(samples)
    deadline = clock.now() + seconds
    while True:
        remaining = deadline - clock.now()
        if remaining <= 0:
            raise NoSample(
                f"{source.describe()}: no new sample with a value within "
                f"{fmt_duration(timeout)}.{fmt_last(_values(source, samples))}"
                f"{fmt_log(source.log_path)}",
                signal=source.describe(),
                log_path=source.log_path,
            )
        await clock.sleep(min(POLL_S, remaining))
        samples = await source.fetch(WINDOW_S)
        for t, v in sorted(samples, key=_sample_time):
            if t > baseline and v is not None:
                return source.from_wire(v)
        baseline = max(baseline, _newest(samples))


async def expect(
    source: _Source[Q],
    matcher: Matcher[Q],
    timeout: timedelta,
    hold_for: timedelta | None = None,
    *,
    clock: _Clock = REAL_CLOCK,
) -> None:
    """Wait for a new sample to match, then, with `hold_for`, check that every
    sample for that long matches too (first match starts the hold)."""
    seconds = _timeout_seconds(timeout)
    _check_hold(hold_for, source.hold_max)
    signal = source.describe()
    log = fmt_log(source.log_path)

    samples = await source.fetch(WINDOW_S)
    baseline = _newest(samples)
    deadline = clock.now() + seconds
    saw_new = saw_value = False
    last_value: Q | None = None
    last_change = clock.now()
    t0: float | None = None

    while t0 is None:
        remaining = deadline - clock.now()
        if remaining <= 0:
            last = fmt_last(_values(source, samples))
            if saw_value:
                raise ExpectationFailed(
                    f"{signal} never matched {matcher.describe()} within "
                    f"{fmt_duration(timeout)}.{last}{log}",
                    signal=signal,
                    log_path=source.log_path,
                )
            raise NoSample(
                f"{signal}: no new sample with a value within "
                f"{fmt_duration(timeout)} while waiting for {matcher.describe()}."
                f"{last}{log}",
                signal=signal,
                log_path=source.log_path,
            )
        await clock.sleep(min(POLL_S, remaining))
        samples = await source.fetch(WINDOW_S)
        advanced = False
        for t, v in sorted(samples, key=_sample_time):
            if t <= baseline:
                continue
            baseline = t
            advanced = saw_new = True
            if v is None:
                continue
            saw_value = True
            q = source.from_wire(v)
            last_value = q
            if matcher.matches(q):
                t0 = t
                break
        if advanced:
            last_change = clock.now()
        elif (
            t0 is None
            and saw_new
            and source.stale_after is not None
            and clock.now() - last_change >= source.stale_after
        ):
            raise NoSample(
                f"{signal} stopped producing samples while waiting for "
                f"{matcher.describe()}: no new sample for "
                f"{fmt_duration(source.stale_after)} (last: {fmt_value(last_value)})."
                f"{log}",
                signal=signal,
                log_path=source.log_path,
            )

    if hold_for is None:
        return
    t0_seen = clock.now()
    await clock.sleep(hold_for.total_seconds())
    await _check_hold_window(source, matcher, t0, t0_seen, hold_for, clock=clock)


async def _check_hold_window(
    source: _Source[Q],
    matcher: Matcher[Q],
    t0: float,
    t0_seen: float,
    hold_for: timedelta,
    *,
    clock: _Clock,
) -> None:
    signal = source.describe()
    log = fmt_log(source.log_path)
    end = t0 + hold_for.total_seconds()
    give_up = clock.now() + EXTRA_POLL_S
    while True:
        window = math.ceil(clock.now() - t0_seen) + HOLD_MARGIN_S
        samples = sorted(await source.fetch_hold(window), key=_sample_time)
        if _newest(samples) >= end or clock.now() >= give_up:
            break
        await clock.sleep(POLL_S)

    held = [(t, v) for t, v in samples if t0 <= t <= end]
    values = _values(source, held)
    hold = fmt_duration(hold_for)

    def fail_no_sample(text: str) -> NoSample:
        return NoSample(text, signal=signal, log_path=source.log_path)

    if not held:
        raise fail_no_sample(
            f"{signal} has no samples in its {hold} hold window after T0.{log}"
        )
    prev = t0
    for i, ((t, _), q) in enumerate(zip(held, values, strict=True)):
        if t - prev > GAP_LIMIT_S:
            raise fail_no_sample(
                f"{signal} has a {fmt_duration(t - prev)} gap in its samples at "
                f"{fmt_at(prev - t0)} of a {hold} hold (limit "
                f"{fmt_duration(GAP_LIMIT_S)}).{fmt_last(values[:i])}{log}"
            )
        if q is None:
            raise fail_no_sample(
                f"{signal} had no value at {fmt_at(t - t0)} of a {hold} hold."
                f"{fmt_last(values[: i + 1])}{log}"
            )
        if not matcher.matches(q):
            raise ExpectationFailed(
                f"{signal} matched {matcher.describe()}, then held for "
                f"{fmt_duration(prev - t0)} of the {hold} asked for (hold_for).\n"
                f"  At {fmt_at(t - t0)} it read {fmt_value(q)}."
                f"{fmt_last(values[: i + 1])}{log}",
                signal=signal,
                log_path=source.log_path,
            )
        prev = t
    if end - prev > GAP_LIMIT_S:
        if _newest(samples) < end:
            raise fail_no_sample(
                f"{signal} stopped producing samples at {fmt_at(prev - t0)} of a "
                f"{hold} hold (last: {fmt_value(values[-1])}).{log}"
            )
        raise fail_no_sample(
            f"{signal} has a {fmt_duration(end - prev)} gap in its samples at "
            f"{fmt_at(prev - t0)} of a {hold} hold (limit "
            f"{fmt_duration(GAP_LIMIT_S)}).{fmt_last(values)}{log}"
        )
