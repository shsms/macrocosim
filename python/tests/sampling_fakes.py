"""A fake 1 Hz sampler and clock for the polling-engine tests."""

from __future__ import annotations

from collections.abc import Sequence
from datetime import timedelta
from pathlib import Path

from frequenz.quantities import Power

from macrocosim._sampling import HOLD_MAX_AGGREGATE, HOLD_MAX_COMPONENT, Sample, _Clock


class FakeClock(_Clock):
    """Monotonic time that only moves when the engine sleeps."""

    def __init__(self) -> None:
        self.t = 0.0
        super().__init__(now=lambda: self.t, sleep=self._sleep)

    async def _sleep(self, seconds: float) -> None:
        self.t += seconds


class FakeSource:
    """Samples emitted once per second of fake monotonic time.

    `values[k]` is emitted at monotonic ``start + k`` and stamped ``server_t0 +
    k`` in simulator time, so the two clocks are offset by an arbitrary amount.
    `skip` leaves ticks out (a sampler gap); a value of `STOP` ends the stream
    there. As an aggregate, `fetch` sees only the newest sample, like
    ``/metrics/latest``.
    """

    def __init__(
        self,
        clock: FakeClock,
        values: Sequence[float | None],
        *,
        start: float = 0.6,
        server_t0: float = 1_700_000_000.0,
        skip: Sequence[int] = (),
        aggregate: bool = False,
        label: str = "BatteryInverter 102 power",
        log_path: Path | None = None,
        old: Sequence[float | None] = (),
        hold_override: Sequence[Sample] | None = None,
    ) -> None:
        self.clock = clock
        self.emitted: list[tuple[float, float, float | None]] = []
        for k, v in enumerate(old, start=-len(old)):
            self.emitted.append((start + k - 1, server_t0 + k - 1, v))
        for k, v in enumerate(values):
            if k not in skip:
                self.emitted.append((start + k, server_t0 + k, v))
        self.aggregate = aggregate
        self.stale_after = 3.0 if aggregate else None
        self.hold_max: timedelta = HOLD_MAX_AGGREGATE if aggregate else HOLD_MAX_COMPONENT
        self.from_wire = Power.from_watts
        self.log_path = log_path
        self._label = label
        self._server_offset = server_t0 - start
        self.hold_windows: list[int] = []
        self._hold_override = hold_override

    def describe(self) -> str:
        return self._label

    def _visible(self) -> list[Sample]:
        return [(t, v) for m, t, v in self.emitted if m <= self.clock.t]

    async def fetch(self, window_s: int) -> list[Sample]:
        visible = self._visible()
        if self.aggregate:
            return visible[-1:]
        server_now = self.clock.t + self._server_offset
        return [(t, v) for t, v in visible if t >= server_now - window_s]

    async def fetch_hold(self, window_s: int) -> list[Sample]:
        self.hold_windows.append(window_s)
        if self._hold_override is not None:
            return list(self._hold_override)
        if self.aggregate:
            return self._visible()
        server_now = self.clock.t + self._server_offset
        return [(t, v) for t, v in self._visible() if t >= server_now - window_s]
