"""The metric model: what a metric *is*, separate from where it is read.

A :class:`MetricSpec` names a metric and fixes two facts about it: the
quantity type of its values, and its *kind*. The kind tells the
assertion engine how to check it:

- ``INSTANTANEOUS`` (power, SoC): a settling signal. An expect polls it
  until the matcher holds — or, with ``for_``, requires the matcher on
  every sample across a duration.
- ``CUMULATIVE`` (energy): a monotonic running total. An expect checks
  the matcher once, after waiting for the total to first appear.

Binding a spec to a read gives a :class:`BoundMetric` — everything the
engine needs: the typed read, the kind, and a label for failure
messages. The catalog at the bottom is the single place a metric is
described; the named ``expect`` methods are one-line sugar over it.
"""

from __future__ import annotations

from collections.abc import Callable
from dataclasses import dataclass
from enum import Enum
from typing import Generic, TypeVar

from frequenz.quantities import Energy, Percentage, Power, Quantity

Q = TypeVar("Q", bound=Quantity)


class MetricKind(Enum):
    """How a metric behaves over time, and so how to assert on it."""

    INSTANTANEOUS = "instantaneous"
    """A momentary signal (power, SoC): settle-then-check semantics."""

    CUMULATIVE = "cumulative"
    """A monotonic running total (energy): checked once, when available."""


@dataclass(frozen=True)
class MetricSpec(Generic[Q]):
    """A metric's identity: its name, kind, and quantity type."""

    name: str
    """The metric's name, also the default assertion label."""

    kind: MetricKind
    """How the metric behaves over time (drives assertion semantics)."""

    quantity: type[Q]
    """The ``frequenz-quantities`` type of its values."""

    def bind(
        self, read: Callable[[], Q | None], *, label: str | None = None
    ) -> BoundMetric[Q]:
        """Bind this spec to a concrete read.

        Args:
            read: returns the latest value, or ``None`` while unavailable.
            label: failure-message label; defaults to the metric name.

        Returns:
            The bound metric, ready for the assertion engine.
        """
        return BoundMetric(spec=self, read=read, label=label or self.name)


@dataclass(frozen=True)
class BoundMetric(Generic[Q]):
    """A metric spec bound to a concrete read on a running site."""

    spec: MetricSpec[Q]
    """What the metric is (name, kind, quantity type)."""

    read: Callable[[], Q | None]
    """Returns the latest value, or ``None`` while unavailable."""

    label: str
    """Identifies the metric in assertion failure messages."""


# --- the catalog: one line per metric the client knows -------------------

ACTIVE_POWER = MetricSpec("active_power", MetricKind.INSTANTANEOUS, Power)
SOC = MetricSpec("soc", MetricKind.INSTANTANEOUS, Percentage)

GRID_POWER = MetricSpec("grid_power", MetricKind.INSTANTANEOUS, Power)
PV_POWER = MetricSpec("pv_power", MetricKind.INSTANTANEOUS, Power)
CONSUMER_POWER = MetricSpec("consumer_power", MetricKind.INSTANTANEOUS, Power)
BATTERY_POWER = MetricSpec("battery_power", MetricKind.INSTANTANEOUS, Power)

GRID_ENERGY = MetricSpec("grid_energy", MetricKind.CUMULATIVE, Energy)
CONSUMER_ENERGY = MetricSpec("consumer_energy", MetricKind.CUMULATIVE, Energy)
PV_ENERGY = MetricSpec("pv_energy", MetricKind.CUMULATIVE, Energy)
BATTERY_ENERGY = MetricSpec("battery_energy", MetricKind.CUMULATIVE, Energy)
