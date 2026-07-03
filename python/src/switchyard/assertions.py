"""Settle-aware, fluent assertions over a running site.

The live sim is real-time, so assertions read a value repeatedly and check
a *settled-state invariant with tolerance* rather than a single transient
sample (see ``docs/e2e-testing.md``). Reach these through the
handle ``expect`` surfaces (``site.expect`` / ``site[id].expect``):

    site.expect.grid_power(max=Power.from_megawatts(1),
                           for_=timedelta(seconds=30))
    site.component(bat).expect.soc(within=(Percentage.from_percent(45),
                                    Percentage.from_percent(55)))

``eventually`` polls until the predicate holds (or the timeout passes);
``always`` requires it on every sample across a duration. ``approx`` /
``within`` / ``max`` / ``min`` are settle-then-check shorthands. Matchers are
typed ``frequenz-quantities`` of the metric's kind (``Power`` for power,
``Percentage`` for SoC), durations are ``datetime.timedelta``. Failures raise
``AssertionError`` carrying the observed value(s).
"""

from __future__ import annotations

import time
from collections.abc import Callable
from datetime import timedelta
from typing import TYPE_CHECKING, Generic, TypeVar

from frequenz.quantities import Quantity

if TYPE_CHECKING:
    pass

Q = TypeVar("Q", bound=Quantity)

_DEFAULT_POLL = timedelta(milliseconds=250)


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

    def __init__(self, read: Callable[[], Q | None], label: str) -> None:
        self._read = read
        self._label = label

    def eventually(
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
        """Poll until the predicate holds, or fail at ``timeout``."""
        pred, desc = _predicate(within, approx, tol, max, min)
        deadline = time.monotonic() + timeout.total_seconds()
        interval = poll.total_seconds()
        value = self._read()
        while not pred(value) and time.monotonic() < deadline:
            time.sleep(interval)
            value = self._read()
        if not pred(value):
            raise AssertionError(
                f"{self._label}: expected {desc} within {timeout}; last read {value}"
            )
        return value

    def always(
        self,
        *,
        within: tuple[Q, Q] | None = None,
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
        pred, desc = _predicate(within, None, None, max, min)
        self._read()  # prime: pay any stream-open latency before the window
        end = time.monotonic() + for_.total_seconds()
        interval = poll.total_seconds()
        series: list[Q | None] = []
        while time.monotonic() < end:
            value = self._read()
            if value is not None:
                series.append(value)
                if not pred(value):
                    raise AssertionError(
                        f"{self._label}: {desc} broke at {value} after "
                        f"{len(series)} sample(s); series={series}"
                    )
            time.sleep(interval)
        return series

    # --- settle-then-check shorthands (bounded poll, then assert) ---------

    def approx(
        self,
        expected: Q,
        *,
        tol: Q,
        timeout: timedelta = timedelta(seconds=5),
        poll: timedelta = _DEFAULT_POLL,
    ) -> Q | None:
        return self.eventually(approx=expected, tol=tol, timeout=timeout, poll=poll)

    def within(
        self,
        bounds: tuple[Q, Q],
        *,
        timeout: timedelta = timedelta(seconds=5),
        poll: timedelta = _DEFAULT_POLL,
    ) -> Q | None:
        return self.eventually(within=bounds, timeout=timeout, poll=poll)

    def max(
        self,
        ceiling: Q,
        *,
        timeout: timedelta = timedelta(seconds=5),
        poll: timedelta = _DEFAULT_POLL,
    ) -> Q | None:
        return self.eventually(max=ceiling, timeout=timeout, poll=poll)

    def min(
        self,
        floor: Q,
        *,
        timeout: timedelta = timedelta(seconds=5),
        poll: timedelta = _DEFAULT_POLL,
    ) -> Q | None:
        return self.eventually(min=floor, timeout=timeout, poll=poll)
