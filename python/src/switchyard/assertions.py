"""Settle-aware, fluent assertions over a running site.

The live sim is real-time, so assertions read a value repeatedly and check
a *settled-state invariant with tolerance* rather than a single transient
sample (see ``docs/e2e-testing.md``). They are ``async`` — each read runs in
a worker thread (``asyncio.to_thread``) and the polls await between reads, so
an app under test on the same event loop keeps running while they settle,
even when a read itself blocks (a sync HTTP round-trip, or a gRPC stream's
first-sample wait). Reach these through the handle ``expect`` surfaces (``site.expect`` /
``site[id].expect``):

    await site.expect.grid_power(max=Power.from_megawatts(1),
                                 for_=timedelta(seconds=30))
    await site.component(bat).expect.soc(within=(Percentage.from_percent(45),
                                         Percentage.from_percent(55)))

``eventually`` polls until the predicate holds (or the timeout passes);
``always`` requires it on every sample across a duration. ``approx`` /
``within`` / ``max`` / ``min`` are settle-then-check shorthands. Matchers are
typed ``frequenz-quantities`` of the metric's kind (``Power`` for power,
``Percentage`` for SoC), durations are ``datetime.timedelta``. Failures raise
``AssertionError`` carrying the observed value(s).
"""

from __future__ import annotations

import asyncio
import inspect
import time
from collections.abc import Callable
from datetime import timedelta
from typing import Generic, TypeVar

from frequenz.quantities import Quantity

from .metrics import BoundMetric, MetricKind, MetricRead

Q = TypeVar("Q", bound=Quantity)

_DEFAULT_POLL = timedelta(milliseconds=250)
_DEFAULT_TIMEOUT = timedelta(seconds=10)


async def _read_value(read: MetricRead[Q]) -> Q | None:
    """Run a read without stalling the event loop.

    An async read (the aio core) is awaited directly on the caller's
    loop. A plain blocking read (the sync facade's transports) runs in a
    worker thread. Either way the app under test keeps running.
    """
    if inspect.iscoroutinefunction(read):
        return await read()
    value = await asyncio.to_thread(read)
    if inspect.isawaitable(value):
        return await value
    return value


def _predicate(
    within: tuple[Q, Q] | None,
    approx: Q | None,
    tol: Q | None,
    max: Q | None,
    min: Q | None,
) -> tuple[Callable[[Q | None], bool], str]:
    """Build a value predicate + a human description from matcher quantities."""
    if within is not None:
        low, high = within
        return (lambda v: v is not None and low <= v <= high, f"within [{low}, {high}]")
    if approx is not None:
        if tol is None:
            raise ValueError("approx=… needs a tol=… (else any value would pass)")
        return (
            lambda v: v is not None and abs(v - approx) <= tol,
            f"≈ {approx} ± {tol}",
        )
    if min is None and max is None:
        raise ValueError(
            "no matcher given: pass within=, approx= with tol=, max=, or min="
        )
    return (
        lambda v: (
            v is not None and (min is None or v >= min) and (max is None or v <= max)
        ),
        f"in [{min}, {max}]",
    )


class Assertion(Generic[Q]):
    """A metric read plus the matchers that assert on it."""

    def __init__(self, read: MetricRead[Q], label: str) -> None:
        self._read = read
        self._label = label

    async def _poll_until(
        self,
        keep_polling: Callable[[Q | None], bool],
        pred: Callable[[Q | None], bool],
        fail: Callable[[Q | None], str],
        *,
        timeout: timedelta,
        poll: timedelta,
    ) -> Q | None:
        """Re-read while ``keep_polling`` holds and the timeout hasn't passed,
        then check ``pred`` on the final value. Reads go through
        ``_read_value`` (await async reads; thread plain ones), so an app
        under test on the same event loop keeps running. Raises
        ``AssertionError(fail(value))`` on a miss. Shared by ``eventually``
        (poll until the matcher passes) and ``once`` (poll only until a value
        is available, then check the matcher a single time).
        """
        deadline = time.monotonic() + timeout.total_seconds()
        interval = poll.total_seconds()
        value = await _read_value(self._read)
        while keep_polling(value) and time.monotonic() < deadline:
            await asyncio.sleep(interval)
            value = await _read_value(self._read)
        if not pred(value):
            raise AssertionError(fail(value))
        return value

    async def eventually(
        self,
        *,
        within: tuple[Q, Q] | None = None,
        approx: Q | None = None,
        tol: Q | None = None,
        max: Q | None = None,
        min: Q | None = None,
        timeout: timedelta = timedelta(seconds=10),
        poll: timedelta = _DEFAULT_POLL,
    ) -> Q | None:
        """Poll until the predicate holds, or fail at ``timeout``.

        Awaits between reads, so an app under test running on the same event
        loop keeps making progress while this settles.
        """
        pred, desc = _predicate(within, approx, tol, max, min)
        return await self._poll_until(
            lambda v: not pred(v),
            pred,
            lambda v: f"{self._label}: expected {desc} within {timeout}; last read {v}",
            timeout=timeout,
            poll=poll,
        )

    async def always(
        self,
        *,
        within: tuple[Q, Q] | None = None,
        approx: Q | None = None,
        tol: Q | None = None,
        max: Q | None = None,
        min: Q | None = None,
        for_: timedelta = timedelta(seconds=2),
        poll: timedelta = _DEFAULT_POLL,
    ) -> list[Q | None]:
        """Require the predicate on every (non-``None``) sample across ``for_``.

        A first read warms the source (opening a stream can block briefly) so
        that latency isn't counted against the window; ``None`` reads (data
        not yet published, or a transient miss) are skipped, not treated as a
        breach — only a real out-of-bounds value fails.
        """
        pred, desc = _predicate(within, approx, tol, max, min)
        # Prime: pay any stream-open latency before the window.
        await _read_value(self._read)
        end = time.monotonic() + for_.total_seconds()
        interval = poll.total_seconds()
        series: list[Q | None] = []
        while time.monotonic() < end:
            value = await _read_value(self._read)
            if value is not None:
                series.append(value)
                if not pred(value):
                    raise AssertionError(
                        f"{self._label}: {desc} broke at {value} after "
                        f"{len(series)} sample(s); series={series}"
                    )
            await asyncio.sleep(interval)
        return series

    async def once(
        self,
        *,
        within: tuple[Q, Q] | None = None,
        approx: Q | None = None,
        tol: Q | None = None,
        max: Q | None = None,
        min: Q | None = None,
        timeout: timedelta = timedelta(seconds=5),
        poll: timedelta = _DEFAULT_POLL,
    ) -> Q | None:
        """Check the matcher on a single reading, once the value is available.

        For a cumulative / monotonic signal (e.g. energy) that should be
        asserted *at a point* rather than settled to: unlike ``eventually``
        this does not poll the matcher — polling a monotonic value against a
        ``max`` would pass early then break as it grows. It polls only the
        value's *availability*, awaiting up to ``timeout`` for the first
        non-``None`` reading (the stream may not have published yet just
        after launch, or right after a topology rebuild cleared the cache),
        then checks the matcher exactly once.
        """
        pred, desc = _predicate(within, approx, tol, max, min)
        return await self._poll_until(
            lambda v: v is None,
            pred,
            lambda v: f"{self._label}: expected {desc}; measured {v}",
            timeout=timeout,
            poll=poll,
        )

    # --- settle-then-check shorthands (bounded poll, then assert) ---------

    async def approx(
        self,
        expected: Q,
        *,
        tol: Q,
        timeout: timedelta = timedelta(seconds=5),
        poll: timedelta = _DEFAULT_POLL,
    ) -> Q | None:
        return await self.eventually(approx=expected, tol=tol, timeout=timeout, poll=poll)

    async def within(
        self,
        bounds: tuple[Q, Q],
        *,
        timeout: timedelta = timedelta(seconds=5),
        poll: timedelta = _DEFAULT_POLL,
    ) -> Q | None:
        return await self.eventually(within=bounds, timeout=timeout, poll=poll)

    async def max(
        self,
        ceiling: Q,
        *,
        timeout: timedelta = timedelta(seconds=5),
        poll: timedelta = _DEFAULT_POLL,
    ) -> Q | None:
        return await self.eventually(max=ceiling, timeout=timeout, poll=poll)

    async def min(
        self,
        floor: Q,
        *,
        timeout: timedelta = timedelta(seconds=5),
        poll: timedelta = _DEFAULT_POLL,
    ) -> Q | None:
        return await self.eventually(min=floor, timeout=timeout, poll=poll)


async def expect_metric(
    metric: BoundMetric[Q],
    *,
    approx: Q | None = None,
    tol: Q | None = None,
    within: tuple[Q, Q] | None = None,
    max: Q | None = None,
    min: Q | None = None,
    for_: timedelta | None = None,
    timeout: timedelta = _DEFAULT_TIMEOUT,
    poll: timedelta = _DEFAULT_POLL,
) -> Q | list[Q | None] | None:
    """Assert on a bound metric; its *kind* picks the check semantics.

    The one engine behind every named ``expect`` method:

    - An ``INSTANTANEOUS`` metric settles: poll until the matcher holds
      (``eventually``), or require it on every sample when ``for_`` is
      given (``always``).
    - A ``CUMULATIVE`` metric is a monotonic total: check the matcher
      once, after awaiting the total's first appearance (``once``).
      ``for_`` does not apply and raises.

    Args:
        metric: the bound metric (read + kind + label).
        approx: match values equal to this, within ``tol``.
        tol: the tolerance for ``approx``.
        within: match values inside this closed interval.
        max: match values at or below this.
        min: match values at or above this.
        for_: require the matcher on every sample across this duration
            (instantaneous metrics only). The window itself is then the
            whole budget: ``timeout`` does not apply.
        timeout: how long to poll (or await availability) before failing.
        poll: the delay between reads.

    Returns:
        The value converged on (or the observed series with ``for_``).

    Raises:
        ValueError: on ``for_`` with a cumulative metric.
    """
    assertion: Assertion[Q] = Assertion(metric.read, metric.label)
    if metric.spec.kind is MetricKind.CUMULATIVE:
        if for_ is not None:
            raise ValueError(
                f"{metric.label}: for_= does not apply to a cumulative "
                "metric — the running total is checked once"
            )
        return await assertion.once(
            within=within,
            approx=approx,
            tol=tol,
            max=max,
            min=min,
            timeout=timeout,
            poll=poll,
        )
    if for_ is not None:
        return await assertion.always(
            within=within,
            approx=approx,
            tol=tol,
            max=max,
            min=min,
            for_=for_,
            poll=poll,
        )
    return await assertion.eventually(
        within=within,
        approx=approx,
        tol=tol,
        max=max,
        min=min,
        timeout=timeout,
        poll=poll,
    )
