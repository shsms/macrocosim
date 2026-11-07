"""Live components: what `microgrid[spec]` and `microgrid[id]` return.

Internal; imports `_enums`, `_signals`, `_spec` and `_units`, and `_session` for
typing only. Every live class has its `id`, the three write-only fault knobs and
its operational mode; the rest are the signals its category publishes or
accepts, and an EV charger's plug. A test only observes what the app under test
commands: there are no setpoint calls here.
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Any

from frequenz.quantities import Current, Energy, Percentage, Power, ReactivePower

from . import _spec
from ._enums import CommandMode, Health, OperationalMode, TelemetryMode
from ._signals import (
    COMMAND_MODE,
    HEALTH,
    INVERTER_POWER,
    OPERATIONAL_MODE,
    POWER,
    PRESSURE,
    REACTIVE,
    SOC,
    STEAM_DEMAND,
    SUNLIGHT,
    TELEMETRY_MODE,
    ClearableKnob,
    ClearableSignal,
    Knob,
    SettableSignal,
    Signal,
    _component_path,
    _ComponentHistorySource,
    _KnobDef,
    _Setter,
    _SignalDef,
    _Writer,
    wire_body,
    wire_value,
)
from ._units import MassFlow, Pressure, is_int_like

if TYPE_CHECKING:
    from ._session import _Session


def _component_repr(self: Component) -> str:
    return f"<{type(self).__name__} {self._id}>"


class Component:
    """A launched component. Never constructed directly; see `Microgrid`."""

    def __init__(self, session: _Session, component_id: int):
        self._session = session
        self._id = component_id
        self._health: Knob[Health] = self._knob(HEALTH, "health")
        self._telemetry_mode: Knob[TelemetryMode] = self._knob(
            TELEMETRY_MODE, "telemetry_mode"
        )
        self._command_mode: Knob[CommandMode] = self._knob(COMMAND_MODE, "command_mode")
        self._operational_mode: Knob[OperationalMode] = self._knob(
            OPERATIONAL_MODE, "operational_mode"
        )

    @property
    def id(self) -> int:
        """The component's id in the simulator."""
        return self._id

    @property
    def health(self) -> Knob[Health]:
        """Write-only. Any health other than `OK` makes the simulator refuse the
        app's commands to this component.

        An inverter that is not `OK` stops: its power goes to 0. An inverter
        shares its power among its batteries that are `OK`, and publishes 0 when
        none is. A meter keeps measuring.

        `ERROR` also sets `command_mode` to `ERROR`. `OK` sets it back to
        `NORMAL` if the component's operational mode accepts control, else to
        `ERROR`. `STANDBY` leaves it alone. So set `command_mode` after
        `health`.
        """
        return self._health

    @property
    def telemetry_mode(self) -> Knob[TelemetryMode]:
        """Write-only. Changes the telemetry the app under test receives over
        gRPC.

        This test's reads of the component do not see the change, because they
        come from the simulator's own sampler. The microgrid's aggregates do see
        it: they are computed from that telemetry.
        """
        return self._telemetry_mode

    @property
    def command_mode(self) -> Knob[CommandMode]:
        """Write-only. How the component answers the app's commands."""
        return self._command_mode

    @property
    def operational_mode(self) -> Knob[OperationalMode]:
        """Write-only. What the component offers: telemetry, control, both or
        neither.

        A `set` also sets `telemetry_mode` to `NORMAL`, or `SILENT` without
        telemetry, and `command_mode` to `NORMAL`, or `ERROR` without control or
        while `health` is `ERROR`. It changes the running component only; the
        spec keeps its value.
        """
        return self._operational_mode

    def _label(self, attribute: str) -> str:
        return f"{type(self).__name__} {self._id} {attribute}"

    def _writer(self, cls: type, attribute: str, kind: type, setter: _Setter) -> _Writer:
        clearable = issubclass(cls, (ClearableSignal, ClearableKnob))
        if clearable and setter.clear_flag is None:
            raise TypeError(f"{self._label(attribute)} has no clear")
        return _Writer(self._session, self._id, self._label(attribute), kind, setter)

    def _source(
        self, definition: _SignalDef, attribute: str
    ) -> _ComponentHistorySource[Any]:
        return _ComponentHistorySource(
            self._session,
            self._id,
            definition.metric,
            definition.kind,
            self._label(attribute),
        )

    def _knob(
        self, definition: _KnobDef, attribute: str, cls: type[Knob[Any]] = Knob
    ) -> Any:
        return cls(self._writer(cls, attribute, definition.kind, definition.setter))

    def _settable(
        self, cls: type[SettableSignal[Any]], definition: _SignalDef, attribute: str
    ) -> Any:
        if definition.setter is None:
            raise TypeError(f"{self._label(attribute)} cannot be set")
        writer = self._writer(cls, attribute, definition.kind, definition.setter)
        return cls(self._source(definition, attribute), writer)

    __repr__ = _component_repr


class GridConnectionPoint(Component):
    """Where the microgrid meets the grid. Read its power through
    `Microgrid.grid_power`."""


class Meter(Component):
    """A meter. Set `power` on a leaf (load) meter to change the load."""

    def __init__(self, session: _Session, component_id: int):
        super().__init__(session, component_id)
        self._power: ClearableSignal[Power] = self._settable(
            ClearableSignal, POWER, "power"
        )
        self._reactive_power: ClearableSignal[ReactivePower] = self._settable(
            ClearableSignal, REACTIVE, "reactive_power"
        )

    @property
    def power(self) -> ClearableSignal[Power]:
        """Active power; positive when the subtree consumes.

        Setting it replaces what the meter measures; `clear()` makes it measure
        its children again (0 W for a leaf meter), not restore the spec's value.
        """
        return self._power

    @property
    def reactive_power(self) -> ClearableSignal[ReactivePower]:
        """Reactive power. `clear()` makes the meter measure its children again,
        and drops any power factor, whether given in the spec or set at runtime
        through the UI or Lisp."""
        return self._reactive_power


class Battery(Component):
    """A battery. It publishes no AC power; read the power of the inverters
    above it."""

    def __init__(self, session: _Session, component_id: int):
        super().__init__(session, component_id)
        self._soc: SettableSignal[Percentage] = self._settable(SettableSignal, SOC, "soc")

    @property
    def soc(self) -> SettableSignal[Percentage]:
        """State of charge. A `set` is a one-time jump; the physics carries on
        from the new value."""
        return self._soc


class BatteryInverter(Component):
    """A battery inverter. Its power is what the app under test commands."""

    def __init__(self, session: _Session, component_id: int):
        super().__init__(session, component_id)
        self._power: Signal[Power] = Signal(self._source(INVERTER_POWER, "power"))

    @property
    def power(self) -> Signal[Power]:
        """Active power, read-only; positive when charging."""
        return self._power


class SolarInverter(Component):
    """A solar inverter. Its power follows its sunlight and the app's limits."""

    def __init__(self, session: _Session, component_id: int):
        super().__init__(session, component_id)
        self._power: Signal[Power] = Signal(self._source(INVERTER_POWER, "power"))
        self._sunlight: ClearableKnob[Percentage] = self._knob(
            SUNLIGHT, "sunlight", ClearableKnob
        )

    @property
    def power(self) -> Signal[Power]:
        """Active power, read-only; negative while producing."""
        return self._power

    @property
    def sunlight(self) -> ClearableKnob[Percentage]:
        """Write-only. The power available, as a share of the array's peak.
        `clear()` makes the inverter follow the site's weather again."""
        return self._sunlight


class Chp(Component):
    """A combined heat and power unit. It has no physics and no signals of its
    own: a leaf that completes the topology. To give it power, set the power of
    the meter above it."""


class EvCharger(Component):
    """An EV charger. Its power is what the plugged car draws, within the limit
    the app under test commands."""

    def __init__(self, session: _Session, component_id: int):
        super().__init__(session, component_id)
        self._power: Signal[Power] = Signal(self._source(INVERTER_POWER, "power"))

    @property
    def power(self) -> Signal[Power]:
        """Active power, read-only; positive while charging."""
        return self._power

    async def plug(
        self,
        preset: str,
        *,
        soc: Percentage | None = None,
        target_soc: Percentage | None = None,
        phases: int | None = None,
        max_current: Current | None = None,
        capacity: Energy | None = None,
        taper_start: Percentage | None = None,
        taper_floor: Percentage | None = None,
    ) -> None:
        """Plug a car in: `preset` names one from the simulator's catalog, and
        the other arguments change its values.

        `target_soc` is 100 % unless given; `phases` is 1, 2 or 3, and another
        int raises `ValueError`. Raises `Rejected` if a car is already plugged
        in, the preset is unknown or a value is out of range.
        """
        label = self._label("plug")
        if is_int_like(phases) and not 1 <= phases <= 3:
            raise ValueError(f"{label}: phases must be 1, 2 or 3, not {phases}")
        body = {"preset": wire_value(f"{label} preset", preset, str)}
        body |= wire_body(
            label,
            {
                "soc": (soc, Percentage),
                "target_soc": (target_soc, Percentage),
                "phases": (phases, int),
                "max_current": (max_current, Current),
                "capacity": (capacity, Energy),
                "taper_start": (taper_start, Percentage),
                "taper_floor": (taper_floor, Percentage),
            },
        )
        await self._session.post_json(
            _component_path(self._session, self._id, "ev"),
            body,
            call=f"{label}({preset!r})",
        )

    async def unplug(self) -> None:
        """Unplug the car, if one is plugged in."""
        await self._session.delete(
            _component_path(self._session, self._id, "ev"),
            call=f"{self._label('unplug')}()",
        )


class SteamBoiler(Component):
    """A hybrid gas and electric steam boiler. The app under test commands its
    electric power; the gas burner holds the pressure at its target."""

    def __init__(self, session: _Session, component_id: int):
        super().__init__(session, component_id)
        self._power: Signal[Power] = Signal(self._source(INVERTER_POWER, "power"))
        self._pressure: SettableSignal[Pressure] = self._settable(
            SettableSignal, PRESSURE, "pressure"
        )
        self._demand: Knob[MassFlow] = self._knob(STEAM_DEMAND, "demand")

    @property
    def power(self) -> Signal[Power]:
        """Electric power, read-only; positive when consuming."""
        return self._power

    @property
    def pressure(self) -> SettableSignal[Pressure]:
        """Steam pressure. A `set` is a one-time jump; the physics carries on
        from the new value."""
        return self._pressure

    @property
    def demand(self) -> Knob[MassFlow]:
        """Write-only. The steam the site draws."""
        return self._demand


class WindTurbine(Component):
    """A wind turbine. It has no physics and no signals of its own: a leaf that
    completes the topology. To give it power, set the power of the meter above
    it."""


class PowerTransformer(Component):
    """A power transformer. It has no physics and no signals of its own: it
    passes its successors' power up to the meter above it."""


class Breaker(Component):
    """A breaker. It has no physics and no signals of its own: it passes its
    successors' power up to the meter above it."""


LIVE_CLASS: dict[type, type[Component]] = {
    _spec.GridConnectionPointSpec: GridConnectionPoint,
    _spec.MeterSpec: Meter,
    _spec.BatterySpec: Battery,
    _spec.BatteryInverterSpec: BatteryInverter,
    _spec.SolarInverterSpec: SolarInverter,
    _spec.ChpSpec: Chp,
    _spec.EvChargerSpec: EvCharger,
    _spec.SteamBoilerSpec: SteamBoiler,
    _spec.WindTurbineSpec: WindTurbine,
    _spec.PowerTransformerSpec: PowerTransformer,
    _spec.BreakerSpec: Breaker,
}
"""The live class each spec class becomes."""
