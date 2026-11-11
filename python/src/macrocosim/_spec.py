"""Topology specs: inert, frozen descriptions of a microgrid to launch.

Internal; imports `_units`, `_enums` and `_errors`. Each component category is
one frozen, keyword-only dataclass, and its ``make_*`` constructor is the class
under a second name. Specs compare and hash by identity: the same object under
two parents is one shared component.

Every field defaults to ``None``, meaning "use the simulator's default"; the
defaults live in the simulator (``src/spec/defaults.rs``), never here. A field
goes on the wire under its name plus the unit suffix of its type (`json_key`).
"""

from __future__ import annotations

import dataclasses
import typing
from collections.abc import Sequence
from dataclasses import dataclass, field
from datetime import timedelta
from enum import Enum, StrEnum
from typing import TYPE_CHECKING, Any, ClassVar, Generic, TypeVar, cast

from frequenz.quantities import (
    ApparentPower,
    Current,
    Energy,
    Percentage,
    Power,
    ReactivePower,
    Voltage,
)

from ._enums import CommandMode, EvIdle, Health, OperationalMode, TelemetryMode
from ._errors import SpecError
from ._units import (
    MassFlow,
    Pressure,
    check_finite,
    check_type,
    is_int_like,
    repr_or_none,
    suffix_of,
    type_and_value,
    unit_of,
)

if TYPE_CHECKING:
    # Named only in the string generic parameters of the spec classes below.
    from ._components import (  # noqa: F401
        Battery,
        BatteryInverter,
        Breaker,
        Chp,
        EvCharger,
        GridConnectionPoint,
        Meter,
        PowerTransformer,
        SolarInverter,
        SteamBoiler,
        WindTurbine,
    )

L = TypeVar("L", bound=object)
"""The live class a spec turns into once launched."""

FIRST_AUTO_ID = 1000
"""Where automatic component ids start. An implementation detail: the docs
promise no particular number."""

MAX_ID = 2**64 - 1
"""The largest id the simulator reads (its ids are unsigned 64-bit)."""

_NOT_WIRE = frozenset({"id", "successors"})
"""Fields `render` writes itself rather than through `spec_wire_fields`."""

MAX_PORT = 65535


def _spec_repr(spec: ComponentSpec[Any]) -> str:
    parts = ["id=auto" if spec.id is None else f"id={int(spec.id)}"]
    for name in _field_kinds(type(spec)):
        if name == "id":
            continue
        value = getattr(spec, name)
        if value is None:
            continue
        if name == "successors":
            parts.append(f"successors=[{len(value)}]")
        elif isinstance(value, Enum):
            parts.append(f"{name}={type(value).__name__}.{value.name}")
        elif unit_of(value) is not None and not isinstance(value, timedelta):
            parts.append(f"{name}={value}")
        else:
            parts.append(f"{name}={value!r}")
    return f"{type(spec).__name__}({', '.join(parts)})"


def _microgrid_spec_repr(self: MicrogridSpec) -> str:
    head = "auto" if self.id is None else str(int(self.id))
    return f"MicrogridSpec(id={head}, components={len(self._order)})"


@dataclass(frozen=True, eq=False, kw_only=True, repr=False)
class ComponentSpec(Generic[L]):
    """The fields every component category shares."""

    _type: ClassVar[str]
    """The component's ``type`` in the topology JSON."""

    id: int | None = None
    name: str | None = None
    stream_jitter: Percentage | None = None
    operational_mode: OperationalMode | None = None
    health: Health | None = None
    telemetry_mode: TelemetryMode | None = None
    command_mode: CommandMode | None = None

    def __post_init__(self) -> None:
        if type(self) is ComponentSpec:
            raise TypeError(
                "ComponentSpec is the base of the component specs; build one with "
                "a make_* constructor"
            )
        for f, kind in _field_kinds(type(self)).items():
            value = getattr(self, f)
            if value is None:
                continue
            if f == "successors":
                _check_successors(self, value)
                continue
            if f == "id":
                _check_id(self, value)
                continue
            check_type(f, value, kind)
            check_finite(f, value)

    __repr__ = _spec_repr


def _check_id(spec: object, value: object) -> None:
    check_type("id", value, int)
    number = int(cast("int", value))
    if not 0 < number <= MAX_ID:
        shown = f", not {text}" if (text := repr_or_none(number)) else ""
        raise SpecError(
            f"{type(spec).__name__}: id must be between 1 and 2**64 - 1{shown}"
        )


def _check_successors(spec: ComponentSpec[Any], value: object) -> None:
    if isinstance(value, (str, bytes)) or not isinstance(value, Sequence):
        raise TypeError(
            f"successors must be a sequence of specs, not {type(value).__name__}"
        )
    children = tuple(value)
    for child in children:
        if not isinstance(child, ComponentSpec):
            raise TypeError(
                f"successors must hold component specs, not {type_and_value(child)}"
            )
    if len({id(c) for c in children}) != len(children):
        twice = next(c for c in children if sum(x is c for x in children) > 1)
        raise SpecError(f"{spec!r}: {twice!r} appears twice in its successors")
    object.__setattr__(spec, "successors", children)


@dataclass(frozen=True, eq=False, kw_only=True, repr=False)
class GridConnectionPointSpec(ComponentSpec["GridConnectionPoint"]):
    """Where the microgrid meets the grid; the root of every topology."""

    _type: ClassVar[str] = "grid_connection_point"

    rated_fuse_current: Current | None = None
    rated_lower: Power | None = None
    rated_upper: Power | None = None
    successors: Sequence[ComponentSpec[Any]] | None = None

    def __post_init__(self) -> None:
        super().__post_init__()
        fuse = self.rated_fuse_current
        if fuse is not None and not float(fuse.as_amperes()).is_integer():
            raise SpecError(
                f"{self!r}: rated_fuse_current must be whole amperes, not {fuse}"
            )


@dataclass(frozen=True, eq=False, kw_only=True, repr=False)
class MeterSpec(ComponentSpec["Meter"]):
    """A meter. A leaf meter with `power` set stands for a load."""

    _type: ClassVar[str] = "meter"

    interval: timedelta | None = None
    power: Power | None = None
    reactive_power: ReactivePower | None = None
    power_factor: float | None = None
    leading: bool | None = None
    hidden: bool | None = None
    successors: Sequence[ComponentSpec[Any]] | None = None

    def __post_init__(self) -> None:
        super().__post_init__()
        if self.reactive_power is not None and self.power_factor is not None:
            raise SpecError(
                f"{self!r}: reactive_power and power_factor cannot both be set"
            )
        if self.leading is not None and self.power_factor is None:
            raise SpecError(f"{self!r}: leading needs power_factor")


@dataclass(frozen=True, eq=False, kw_only=True, repr=False)
class BatterySpec(ComponentSpec["Battery"]):
    """A battery, under one or more battery inverters."""

    _type: ClassVar[str] = "battery"

    interval: timedelta | None = None
    capacity: Energy | None = None
    initial_soc: Percentage | None = None
    soc_lower: Percentage | None = None
    soc_upper: Percentage | None = None
    soc_protect_margin: Percentage | None = None
    voltage: Voltage | None = None
    rated_lower: Power | None = None
    rated_upper: Power | None = None


@dataclass(frozen=True, eq=False, kw_only=True, repr=False)
class BatteryInverterSpec(ComponentSpec["BatteryInverter"]):
    """A battery inverter; its successors are batteries."""

    _type: ClassVar[str] = "battery_inverter"

    interval: timedelta | None = None
    successors: Sequence[ComponentSpec[Any]] | None = None
    rated_lower: Power | None = None
    rated_upper: Power | None = None
    command_delay: timedelta | None = None
    device_delay: timedelta | None = None
    reactive_command_delay: timedelta | None = None
    ramp_rate_w_per_s: float | None = None
    reactive_pf_limit: float | None = None
    reactive_apparent: ApparentPower | None = None
    reactive_ramp_rate_var_per_s: float | None = None


@dataclass(frozen=True, eq=False, kw_only=True, repr=False)
class SolarInverterSpec(ComponentSpec["SolarInverter"]):
    """A solar inverter with its array. Without `sunlight` it follows the site's
    weather; with it, it holds that sunlight until cleared."""

    _type: ClassVar[str] = "solar_inverter"

    interval: timedelta | None = None
    rated_lower: Power | None = None
    rated_upper: Power | None = None
    command_delay: timedelta | None = None
    device_delay: timedelta | None = None
    reactive_command_delay: timedelta | None = None
    ramp_rate_w_per_s: float | None = None
    reactive_pf_limit: float | None = None
    reactive_apparent: ApparentPower | None = None
    reactive_ramp_rate_var_per_s: float | None = None
    sunlight: Percentage | None = None
    array_peak: Power | None = None
    weather_lag: timedelta | None = None
    weather_jitter: Percentage | None = None


@dataclass(frozen=True, eq=False, kw_only=True, repr=False)
class ChpSpec(ComponentSpec["Chp"]):
    """A combined heat and power unit: a passive part of the topology."""

    _type: ClassVar[str] = "chp"


@dataclass(frozen=True, eq=False, kw_only=True, repr=False)
class EvChargerSpec(ComponentSpec["EvCharger"]):
    """An EV charger. It starts with no car plugged in."""

    _type: ClassVar[str] = "ev_charger"

    interval: timedelta | None = None
    rated_lower: Power | None = None
    rated_upper: Power | None = None
    command_delay: timedelta | None = None
    ramp_rate_w_per_s: float | None = None
    device_delay: timedelta | None = None
    phases: int | None = None
    """The phases it is wired on: 1 or 3."""
    idle: EvIdle | None = None
    resume_on_recovery: bool | None = None
    """Keep the command through a health fault and ramp back to it."""

    def __post_init__(self) -> None:
        super().__post_init__()
        if self.phases is not None and int(self.phases) not in (1, 3):
            raise SpecError(f"{self!r}: phases must be 1 or 3, not {int(self.phases)}")


@dataclass(frozen=True, eq=False, kw_only=True, repr=False)
class SteamBoilerSpec(ComponentSpec["SteamBoiler"]):
    """A hybrid gas and electric steam boiler. Without `initial` it starts at
    its `target` pressure."""

    _type: ClassVar[str] = "steam_boiler"

    interval: timedelta | None = None
    demand: MassFlow | None = None
    """The steam demand."""
    rated_lower: Power | None = None
    rated_upper: Power | None = None
    target: Pressure | None = None
    """The pressure the gas burner holds; above 0."""
    max: Pressure | None = None
    """The highest pressure; at least `target`."""
    initial: Pressure | None = None
    capacity_wh_per_bar: float | None = None
    """Energy to raise the pressure by 1 bar."""
    wh_per_kg: float | None = None
    """Energy to make 1 kg of steam."""
    command_delay: timedelta | None = None
    ramp_rate_w_per_s: float | None = None
    device_delay: timedelta | None = None


@dataclass(frozen=True, eq=False, kw_only=True, repr=False)
class WindTurbineSpec(ComponentSpec["WindTurbine"]):
    """A wind turbine. It has no physics of its own and produces no power; like
    a CHP, it marks what the meter above it measures."""

    _type: ClassVar[str] = "wind_turbine"


@dataclass(frozen=True, eq=False, kw_only=True, repr=False)
class PowerTransformerSpec(ComponentSpec["PowerTransformer"]):
    """A power transformer: passive, it passes its successors' power up."""

    _type: ClassVar[str] = "power_transformer"

    successors: Sequence[ComponentSpec[Any]] | None = None


@dataclass(frozen=True, eq=False, kw_only=True, repr=False)
class BreakerSpec(ComponentSpec["Breaker"]):
    """A breaker: passive, it passes its successors' power up."""

    _type: ClassVar[str] = "breaker"

    successors: Sequence[ComponentSpec[Any]] | None = None


@dataclass(frozen=True, eq=False, kw_only=True, repr=False)
class MicrogridSpec:
    """A microgrid: its id, name and TSO, and its topology, rooted at the grid
    connection point. Building one picks an id for each component spec that has
    none; the specs themselves do not change.

    `id` and `grpc_port` left out are chosen by the simulator; `physics_tick`
    left out is 0.1 s."""

    id: int | None = None
    name: str | None = None
    tso: str | None = None
    grpc_port: int | None = None
    physics_tick: timedelta | None = None
    topology: GridConnectionPointSpec

    _order: tuple[ComponentSpec[Any], ...] = field(init=False, default=())
    """Every component spec once, children before parents."""

    _ids: dict[ComponentSpec[Any], int] = field(init=False, default_factory=dict)
    """The id of each component spec, explicit or assigned."""

    def __post_init__(self) -> None:
        if self.id is not None:
            _check_id(self, self.id)
        for f in ("name", "tso"):
            value = getattr(self, f)
            if value is not None:
                check_type(f, value, str)
        if self.grpc_port is not None:
            check_type("grpc_port", self.grpc_port, int)
            if not 0 < int(self.grpc_port) <= MAX_PORT:
                raise SpecError(
                    f"MicrogridSpec: grpc_port must be between 1 and {MAX_PORT}, "
                    f"not {int(self.grpc_port)}"
                )
        if self.physics_tick is not None:
            check_type("physics_tick", self.physics_tick, timedelta)
            if self.physics_tick <= timedelta(0):
                tick = self.physics_tick
                raise SpecError(
                    f"MicrogridSpec: physics_tick must be positive, not {tick}"
                )
        if not isinstance(self.topology, GridConnectionPointSpec):
            raise TypeError(
                "topology must be a GridConnectionPointSpec (make_grid_connection_point),"
                f" not {type(self.topology).__name__}"
            )
        order, ids, _ = _assign_ids(self.topology)
        object.__setattr__(self, "_order", order)
        object.__setattr__(self, "_ids", ids)

    __repr__ = _microgrid_spec_repr


make_microgrid = MicrogridSpec
make_grid_connection_point = GridConnectionPointSpec
make_meter = MeterSpec
make_battery = BatterySpec
make_battery_inverter = BatteryInverterSpec
make_solar_inverter = SolarInverterSpec
make_chp = ChpSpec
make_ev_charger = EvChargerSpec
make_steam_boiler = SteamBoilerSpec
make_wind_turbine = WindTurbineSpec
make_power_transformer = PowerTransformerSpec
make_breaker = BreakerSpec


def _assign_ids(
    root: GridConnectionPointSpec,
    start: int = FIRST_AUTO_ID,
    skip: frozenset[int] = frozenset(),
) -> tuple[tuple[ComponentSpec[Any], ...], dict[ComponentSpec[Any], int], int]:
    """Order the tree children-first and map every spec to its id: its own `id`,
    or a new one for a spec without. Also returns where the next tree's
    automatic ids continue.

    Automatic ids count up from `start`, and from past each explicit id met
    before them. An automatic id never equals an explicit one in the same tree
    or one in `skip`, and a spec shared by two parents is visited, and numbered,
    once.
    """
    order: list[ComponentSpec[Any]] = []
    seen: set[int] = set()

    def visit(spec: ComponentSpec[Any]) -> None:
        seen.add(id(spec))
        for child in children(spec):
            if id(child) not in seen:
                visit(child)
        order.append(spec)

    visit(root)

    explicit: dict[int, ComponentSpec[Any]] = {}
    for spec in order:
        if spec.id is None:
            continue
        number = int(spec.id)
        other = explicit.setdefault(number, spec)
        if other is not spec:
            raise SpecError(f"{other!r} and {spec!r} both have id {number}")

    ids: dict[ComponentSpec[Any], int] = {}
    next_id = start
    for spec in order:
        if spec.id is not None:
            ids[spec] = int(spec.id)
            next_id = max(next_id, int(spec.id) + 1)
        else:
            while next_id in explicit or next_id in skip:
                next_id += 1
            if next_id > MAX_ID:
                raise SpecError(
                    f"{spec!r}: the next automatic id would pass 2**64 - 1 (automatic "
                    "ids count up from the largest explicit id numbered before this "
                    "spec, children before parents and siblings in order, and skip "
                    "every explicit id); give this spec an id"
                )
            ids[spec] = next_id
            next_id += 1
    return tuple(order), ids, next_id


def _check_explicit_ids(specs: Sequence[MicrogridSpec]) -> frozenset[int]:
    """The explicit component ids of `specs`; one given in two of them raises
    `SpecError`, since component ids are unique across the microgrids."""
    owner: dict[int, MicrogridSpec] = {}
    for mg in specs:
        for spec in mg._order:
            if spec.id is None:
                continue
            number = int(spec.id)
            other = owner.setdefault(number, mg)
            if other is not mg:
                raise SpecError(
                    f"{spec!r}: id {number} is in {other!r} and in {mg!r}; component "
                    "ids are unique across the microgrids"
                )
    return frozenset(owner)


def number_microgrids(specs: list[MicrogridSpec]) -> None:
    """Number the automatic component ids of `specs` across the whole list, so
    no two microgrids share an id.

    The explicit ids of every microgrid are collected first; two microgrids
    giving the same one raise `SpecError`. Then each microgrid's automatic ids
    are numbered in list order, continuing from where the previous microgrid's
    numbering ended and skipping every explicit id. The specs are renumbered in
    place, so they stay the keys that find their components.
    """
    check_type("specs", specs, list)
    for mg in specs:
        check_type("specs item", mg, MicrogridSpec)
    skip = _check_explicit_ids(specs)
    start = FIRST_AUTO_ID
    for mg in specs:
        order, ids, start = _assign_ids(mg.topology, start, skip)
        object.__setattr__(mg, "_order", order)
        object.__setattr__(mg, "_ids", ids)


def _strip_none(hint: Any) -> type | tuple[type, ...]:
    args = [a for a in typing.get_args(hint) if a is not type(None)]
    if not args:
        return hint
    if len(args) == 1:
        return args[0]
    return tuple(args)


_KINDS: dict[type, dict[str, Any]] = {}


def _field_kinds(cls: type) -> dict[str, Any]:
    """Each init field's type, with ``| None`` removed, in field order."""
    kinds = _KINDS.get(cls)
    if kinds is None:
        hints = typing.get_type_hints(cls)
        kinds = {
            f.name: _strip_none(hints[f.name]) for f in dataclasses.fields(cls) if f.init
        }
        _KINDS[cls] = kinds
    return kinds


def json_key(cls: type, name: str) -> str:
    """The JSON field name for a spec field: its name plus its unit suffix."""
    return _key_for_kind(name, _field_kinds(cls)[name])


def _key_for_kind(name: str, kind: object) -> str:
    suffix = suffix_of(kind) if isinstance(kind, type) else ""
    return name + suffix


def children(spec: ComponentSpec[Any]) -> tuple[ComponentSpec[Any], ...]:
    """The specs listed as `spec`'s successors; empty for a leaf."""
    return tuple(getattr(spec, "successors", None) or ())


def _wire(value: object, kind: object) -> object:
    if isinstance(value, Current):
        return int(value.as_amperes())
    unit = unit_of(value)
    if unit is not None:
        return unit.to_wire(value)
    if isinstance(value, StrEnum):
        return str(value)
    if isinstance(value, (bool, str)):
        return value
    if is_int_like(value):
        number = int(value)
        return float(number) if kind is float else number
    assert isinstance(value, float), value  # __post_init__ checked every field
    return value


def spec_wire_fields(spec: ComponentSpec[Any]) -> list[tuple[str, object]]:
    """The JSON field name and wire value of each field that has a value, in
    field order.

    `id` and `successors` are left out; `render` writes those itself. Values are
    base-unit floats, a whole-ampere `int` for `Current`, a `float` for a
    `float` field given an `int`, an enum member's value in snake case, and
    `bool` and `str` unchanged.
    """
    cls = type(spec)
    pairs: list[tuple[str, object]] = []
    for name, kind in _field_kinds(cls).items():
        if name in _NOT_WIRE:
            continue
        value = getattr(spec, name)
        if value is not None:
            pairs.append((_key_for_kind(name, kind), _wire(value, kind)))
    return pairs
