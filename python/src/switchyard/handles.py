"""Component and microgrid handles — the object-oriented control surface.

``site.component(id)`` (or ``site[id]``) returns a :class:`ComponentHandle`
bound to the running site; act on it by *intent*:

    inv = site.component(3)
    inv.command(active_power=Power.from_kilowatts(2))  # what the app sends (gRPC)
    inv.status(health=Health.ERROR)                    # set operational status
    site[6].drive(power=Power.from_megawatts(2))       # drive the environment
    inv.active_power()                                 # read -> Power | None
    inv.expect.active_power(approx=Power.from_kilowatts(2), tol=Power.from_watts(300))

``command`` issues a control command through the real gRPC gateway (so an
out-of-envelope value raises :class:`~switchyard.errors.SetpointRejected`, the
production behaviour under test); ``status`` / ``drive`` are test-side stimuli
POSTed to ``/api/eval``. Microgrid aggregates live on the site:
``site.expect.grid_power(...)`` (or ``site.microgrid(id)`` for a non-default
one).
"""

from __future__ import annotations

from collections.abc import Callable
from datetime import timedelta
from typing import TYPE_CHECKING, TypeVar

from frequenz.quantities import Quantity

from .assertions import Assertion
from .build import RawLisp, to_lisp_atom

if TYPE_CHECKING:
    from frequenz.quantities import Percentage, Power

    from .build import Component
    from .enums import CommandMode, Health, TelemetryMode
    from .runtime import Site

_Q = TypeVar("_Q", bound=Quantity)

_TIMEOUT = timedelta(seconds=10)
_POLL = timedelta(milliseconds=250)


def _settle(
    read: Callable[[], _Q | None],
    label: str,
    *,
    approx: _Q | None,
    tol: _Q | None,
    within: tuple[_Q, _Q] | None,
    max: _Q | None,
    min: _Q | None,
    for_: timedelta | None,
    timeout: timedelta,
    poll: timedelta,
) -> _Q | list[_Q | None] | None:
    """Poll a read until the matcher holds (``eventually``), or on every sample
    across ``for_`` (``always``). Raises ``AssertionError`` on breach; returns
    the value converged on (or the series observed)."""
    a: Assertion[_Q] = Assertion(read, label)
    if for_ is not None:
        return a.always(within=within, max=max, min=min, for_=for_, poll=poll)
    return a.eventually(
        within=within,
        approx=approx,
        tol=tol,
        max=max,
        min=min,
        timeout=timeout,
        poll=poll,
    )


class ComponentExpect:
    """Settle-aware assertions on one component's telemetry."""

    def __init__(self, site: Site, component_id: int, mg_id: int | None) -> None:
        self._site = site
        self._id = component_id
        self._mg = mg_id

    def active_power(
        self,
        *,
        approx: Power | None = None,
        tol: Power | None = None,
        within: tuple[Power, Power] | None = None,
        max: Power | None = None,
        min: Power | None = None,
        for_: timedelta | None = None,
        timeout: timedelta = _TIMEOUT,
        poll: timedelta = _POLL,
    ) -> Power | list[Power | None] | None:
        return _settle(
            lambda: self._site.active_power(self._id, self._mg),
            f"component {self._id} active_power",
            approx=approx,
            tol=tol,
            within=within,
            max=max,
            min=min,
            for_=for_,
            timeout=timeout,
            poll=poll,
        )

    def soc(
        self,
        *,
        approx: Percentage | None = None,
        tol: Percentage | None = None,
        within: tuple[Percentage, Percentage] | None = None,
        max: Percentage | None = None,
        min: Percentage | None = None,
        for_: timedelta | None = None,
        timeout: timedelta = _TIMEOUT,
        poll: timedelta = _POLL,
    ) -> Percentage | list[Percentage | None] | None:
        return _settle(
            lambda: self._site.soc(self._id, self._mg),
            f"component {self._id} soc",
            approx=approx,
            tol=tol,
            within=within,
            max=max,
            min=min,
            for_=for_,
            timeout=timeout,
            poll=poll,
        )


class ComponentHandle:
    """A component in a running site, acted on by intent."""

    def __init__(self, site: Site, component_id: int, mg_id: int | None = None) -> None:
        self._site = site
        self._id = component_id
        self._mg = mg_id

    def command(
        self,
        *,
        active_power: Power | None = None,
        bounds: tuple[Power, Power] | None = None,
        lifetime: timedelta | None = None,
    ) -> ComponentHandle:
        """Issue a control command as the app would — through the gRPC gateway.

        An ``active_power`` outside the live envelope raises ``SetpointRejected``.
        ``bounds`` augments (narrows) the effective active-power bounds.
        """
        if active_power is not None:
            self._site.set_active_power(
                self._id, active_power, lifetime=lifetime, mg_id=self._mg
            )
        if bounds is not None:
            self._site.augment_bounds(self._id, bounds[0], bounds[1], mg_id=self._mg)
        return self

    def status(
        self,
        *,
        health: Health | None = None,
        command_mode: CommandMode | None = None,
        telemetry_mode: TelemetryMode | None = None,
    ) -> ComponentHandle:
        """Set the component's operational status — its reported health and its
        command / telemetry channel modes (this is how you inject faults)."""
        if health is not None:
            self._eval(f"(set-component-health {self._id} {to_lisp_atom(health)})")
        if command_mode is not None:
            mode = to_lisp_atom(command_mode)
            self._eval(f"(set-component-command-mode {self._id} {mode})")
        if telemetry_mode is not None:
            mode = to_lisp_atom(telemetry_mode)
            self._eval(f"(set-component-telemetry-mode {self._id} {mode})")
        return self

    def drive(
        self,
        *,
        power: Power | RawLisp | None = None,
        sunlight: Percentage | None = None,
    ) -> ComponentHandle:
        """Drive the environment: a meter's published power, a PV's sunlight."""
        if power is not None:
            self._eval(f"(set-meter-power {self._id} {to_lisp_atom(power)})")
        if sunlight is not None:
            self._eval(f"(set-solar-sunlight {self._id} {to_lisp_atom(sunlight)})")
        return self

    def active_power(self) -> Power | None:
        """A single sample of this component's active power (gRPC)."""
        return self._site.active_power(self._id, self._mg)

    def soc(self) -> Percentage | None:
        """A single sample of this battery's state of charge (gRPC)."""
        return self._site.soc(self._id, self._mg)

    @property
    def expect(self) -> ComponentExpect:
        """Settle-aware assertions on this component."""
        return ComponentExpect(self._site, self._id, self._mg)

    def _eval(self, expr: str) -> None:
        self._site.eval(expr, self._mg)


class MicrogridExpect:
    """Settle-aware assertions on a microgrid's graph-derived aggregates."""

    def __init__(self, site: Site, mg_id: int | None) -> None:
        self._site = site
        self._mg = mg_id

    def _formula(
        self,
        name: str,
        read: Callable[[], Power | None],
        approx: Power | None,
        tol: Power | None,
        within: tuple[Power, Power] | None,
        max: Power | None,
        min: Power | None,
        for_: timedelta | None,
        timeout: timedelta,
        poll: timedelta,
    ) -> Power | list[Power | None] | None:
        return _settle(
            read,
            name,
            approx=approx,
            tol=tol,
            within=within,
            max=max,
            min=min,
            for_=for_,
            timeout=timeout,
            poll=poll,
        )

    def grid_power(
        self,
        *,
        approx: Power | None = None,
        tol: Power | None = None,
        within: tuple[Power, Power] | None = None,
        max: Power | None = None,
        min: Power | None = None,
        for_: timedelta | None = None,
        timeout: timedelta = _TIMEOUT,
        poll: timedelta = _POLL,
    ) -> Power | list[Power | None] | None:
        return self._formula(
            "grid_power",
            lambda: self._site.grid_power(self._mg),
            approx,
            tol,
            within,
            max,
            min,
            for_,
            timeout,
            poll,
        )

    def pv_power(
        self,
        *,
        approx: Power | None = None,
        tol: Power | None = None,
        within: tuple[Power, Power] | None = None,
        max: Power | None = None,
        min: Power | None = None,
        for_: timedelta | None = None,
        timeout: timedelta = _TIMEOUT,
        poll: timedelta = _POLL,
    ) -> Power | list[Power | None] | None:
        return self._formula(
            "pv_power",
            lambda: self._site.pv_power(self._mg),
            approx,
            tol,
            within,
            max,
            min,
            for_,
            timeout,
            poll,
        )

    def consumer_power(
        self,
        *,
        approx: Power | None = None,
        tol: Power | None = None,
        within: tuple[Power, Power] | None = None,
        max: Power | None = None,
        min: Power | None = None,
        for_: timedelta | None = None,
        timeout: timedelta = _TIMEOUT,
        poll: timedelta = _POLL,
    ) -> Power | list[Power | None] | None:
        return self._formula(
            "consumer_power",
            lambda: self._site.consumer_power(self._mg),
            approx,
            tol,
            within,
            max,
            min,
            for_,
            timeout,
            poll,
        )

    def battery_power(
        self,
        *,
        approx: Power | None = None,
        tol: Power | None = None,
        within: tuple[Power, Power] | None = None,
        max: Power | None = None,
        min: Power | None = None,
        for_: timedelta | None = None,
        timeout: timedelta = _TIMEOUT,
        poll: timedelta = _POLL,
    ) -> Power | list[Power | None] | None:
        return self._formula(
            "battery_power",
            lambda: self._site.battery_power(self._mg),
            approx,
            tol,
            within,
            max,
            min,
            for_,
            timeout,
            poll,
        )


class MicrogridHandle:
    """A specific microgrid within a running site (for multi-microgrid setups)."""

    def __init__(self, site: Site, mg_id: int) -> None:
        self._site = site
        self._mg = mg_id

    def component(self, target: Component | int) -> ComponentHandle:
        cid = self._site._component_id_of(target)
        return ComponentHandle(self._site, cid, self._mg)

    def __getitem__(self, target: Component | int) -> ComponentHandle:
        return self.component(target)

    @property
    def expect(self) -> MicrogridExpect:
        return MicrogridExpect(self._site, self._mg)

    def grid_power(self) -> Power | None:
        return self._site.grid_power(self._mg)

    def pv_power(self) -> Power | None:
        return self._site.pv_power(self._mg)

    def consumer_power(self) -> Power | None:
        return self._site.consumer_power(self._mg)

    def battery_power(self) -> Power | None:
        return self._site.battery_power(self._mg)
