"""The live microgrid that `launch` yields.

Internal; imports `_components`, `_signals`, `_spec`, `_errors`, `_units`, and
`_session` and `_process` for typing only.
"""

from __future__ import annotations

from datetime import time, timedelta
from pathlib import Path
from typing import TYPE_CHECKING, Any, TypeVar, cast, overload

from frequenz.quantities import Energy, Frequency, Percentage, Power, Voltage

from ._components import LIVE_CLASS, Component
from ._errors import UnknownComponent, fmt_ids
from ._signals import (
    AGGREGATES,
    FREQUENCY_PATH,
    ClearableKnob,
    Signal,
    _AggregateSource,
    _FrequencyWriter,
    wire_body,
    wire_value,
)
from ._spec import ComponentSpec, MicrogridSpec
from ._spec import children as spec_children
from ._units import is_int_like, repr_or_none, type_and_value

if TYPE_CHECKING:
    from ._process import Endpoints
    from ._session import _Session

L = TypeVar("L")


def _microgrid_repr(mg: Microgrid) -> str:
    """The tree, one component per line, with ids."""
    lines = [f"Microgrid {mg.id} at {mg.grpc_url}"]
    ids = mg._spec._ids
    seen: set[int] = set()

    def visit(spec: ComponentSpec[Any], prefix: str, last: bool) -> None:
        branch = "└─ " if last else "├─ "
        name = LIVE_CLASS[type(spec)].__name__
        notes = []
        if spec.id is None:
            notes.append("auto")
        shared = id(spec) in seen
        if shared:
            notes = ["shared, see above"]
        text = f"{name} {ids[spec]}" + (f" ({', '.join(notes)})" if notes else "")
        lines.append(prefix + branch + text)
        if shared:
            return
        seen.add(id(spec))
        children = spec_children(spec)
        child_prefix = prefix + ("   " if last else "│  ")
        for i, child in enumerate(children):
            visit(child, child_prefix, i == len(children) - 1)

    visit(mg._spec.topology, "", True)
    return "\n".join(lines)


def _hhmm(label: str, value: object) -> str:
    """A `time` of day as the weather route's ``"HH:MM"``."""
    t = wire_value(label, value, time)
    if t.second or t.microsecond:
        raise ValueError(f"{label} must be a whole minute, not {t}")
    if t.utcoffset() not in (None, timedelta(0)):
        raise ValueError(f"{label} must be a UTC time, not {t}")
    return f"{t.hour:02}:{t.minute:02}"


def _pair(label: str, value: object, kind: type) -> list[Any] | None:
    """A (low, high) range as the weather route's ``[low, high]``."""
    if value is None:
        return None
    wire_value(label, value, tuple)
    pair = cast("tuple[object, ...]", value)
    if len(pair) != 2:
        raise ValueError(f"{label} must be a (low, high) pair, not {pair!r}")
    return [wire_value(label, v, kind) for v in pair]


class Microgrid:
    """A launched microgrid: its components, aggregates and addresses.

    Built by `launch`; never constructed directly. Look components up with
    `microgrid[spec]` (the object put in the tree) or `microgrid[id]`.
    """

    _session: _Session
    _spec: MicrogridSpec
    _endpoints: Endpoints
    _by_id: dict[int, ComponentSpec[Any]]
    _live: dict[int, Component]
    _aggregates: dict[str, Signal[Any]]
    _frequency: ClearableKnob[Frequency]

    def __init__(self, *args: object, **kwargs: object) -> None:
        raise TypeError(
            "Microgrid is built by macrocosim.launch; describe a microgrid "
            "with make_microgrid"
        )

    @classmethod
    def _create(
        cls,
        session: _Session,
        spec: MicrogridSpec,
        endpoints: Endpoints,
    ) -> Microgrid:
        self = cls.__new__(cls)
        self._session = session
        self._spec = spec
        self._endpoints = endpoints
        self._by_id = {cid: s for s, cid in spec._ids.items()}
        self._live = {}
        self._aggregates = {
            a.name: Signal(_AggregateSource(session, a)) for a in AGGREGATES
        }
        self._frequency = ClearableKnob(
            _FrequencyWriter(session, self._label("frequency"))
        )
        return self

    def _label(self, attribute: str) -> str:
        return f"Microgrid {self.id} {attribute}"

    async def _post(self, rel: str, body: dict[str, Any] | None, call: str) -> None:
        path = self._session.microgrid_path(rel)
        await self._session.post_json(path, body, call=self._label(call))

    @property
    def id(self) -> int:
        """The microgrid's id in the simulator."""
        return self._endpoints.microgrid_id

    @property
    def grpc_url(self) -> str:
        """The Microgrid API address for the app under test
        (``grpc://host:port``)."""
        return f"grpc://{self._endpoints.grpc_addr}"

    @property
    def ui_url(self) -> str:
        """The simulator's web UI (``http://host:port``)."""
        return f"http://{self._endpoints.ui}"

    @property
    def log_path(self) -> Path | None:
        """The simulator log, when `launch` was given a `state_dir` to keep it;
        else ``None``."""
        return self._session.log_path

    def _aggregate(self, name: str) -> Signal[Any]:
        return self._aggregates[name]

    @property
    def grid_power(self) -> Signal[Power]:
        """Power at the grid connection point; positive when importing."""
        return self._aggregate("grid_power")

    @property
    def pv_power(self) -> Signal[Power]:
        """Total solar power; negative while producing."""
        return self._aggregate("pv_power")

    @property
    def consumer_power(self) -> Signal[Power]:
        """Total power of the consumers (loads); positive when consuming."""
        return self._aggregate("consumer_power")

    @property
    def battery_power(self) -> Signal[Power]:
        """Total battery power; positive when charging."""
        return self._aggregate("battery_power")

    @property
    def grid_energy(self) -> Signal[Energy]:
        """The integral of `grid_power` since the simulator's aggregates started
        (not since the test began): import minus export."""
        return self._aggregate("grid_energy")

    @property
    def pv_energy(self) -> Signal[Energy]:
        """The integral of `pv_power` since the aggregates started. It falls
        while the solar inverters produce."""
        return self._aggregate("pv_energy")

    @property
    def consumer_energy(self) -> Signal[Energy]:
        """The integral of `consumer_power` since the aggregates started."""
        return self._aggregate("consumer_energy")

    @property
    def battery_energy(self) -> Signal[Energy]:
        """The integral of `battery_power` since the aggregates started: charged
        minus discharged energy, not the energy stored."""
        return self._aggregate("battery_energy")

    async def set_voltage_per_phase(self, l1: Voltage, l2: Voltage, l3: Voltage) -> None:
        """Set the grid voltage of each phase. Components report it, and meters
        and inverters split their power across the phases with it."""
        label = self._label("set_voltage_per_phase")
        volts = [wire_value(label, v, Voltage) for v in (l1, l2, l3)]
        await self._post(
            "grid", {"voltage_per_phase_v": volts}, f"set_voltage_per_phase{tuple(volts)}"
        )

    async def set_sample_lag(self, lag: timedelta) -> None:
        """Shift the timestamps of every telemetry sample the app receives `lag`
        into the past; ``timedelta(0)`` turns the lag off."""
        seconds = wire_value(self._label("set_sample_lag"), lag, timedelta)
        await self._post(
            "sample-lag", {"sample_lag_s": seconds}, f"set_sample_lag({lag})"
        )

    async def cancel_streams(self) -> None:
        """End every telemetry stream the app has open, with ``CANCELLED``. The
        app can open new ones."""
        await self._post("streams/cancel", None, "cancel_streams()")

    @property
    def frequency(self) -> ClearableKnob[Frequency]:
        """Write-only. The grid frequency, one value for the whole simulator:
        every microgrid in it reads the same.

        `set` holds the frequency at the value given; `clear` hands it back to
        the frequency model (see `set_frequency_model`).
        """
        return self._frequency

    async def set_frequency_model(
        self,
        *,
        nominal: Frequency | None = None,
        mean_rev_rate: float | None = None,
        sigma: float | None = None,
    ) -> None:
        """Change the model that drives the shared grid frequency when no value
        is held: it pulls the frequency toward `nominal` at `mean_rev_rate` per
        second, with noise of `sigma` hertz per square-root second. Arguments
        left out keep their values."""
        label = self._label("set_frequency_model")
        model = wire_body(
            label,
            {
                "nominal": (nominal, Frequency),
                "mean_rev_rate_per_s": (mean_rev_rate, float),
                "sigma_hz_per_sqrt_s": (sigma, float),
            },
        )
        await self._session.post_json(FREQUENCY_PATH, {"model": model}, call=f"{label}()")

    async def set_weather(
        self,
        *,
        sunrise: time | None = None,
        sunset: time | None = None,
        peak: Percentage | None = None,
        cloud_mean_gap: timedelta | None = None,
        cloud_depth: tuple[Percentage, Percentage] | None = None,
        cloud_duration: tuple[timedelta, timedelta] | None = None,
        cloud_ramp: tuple[timedelta, timedelta] | None = None,
        seed: int | None = None,
    ) -> None:
        """Change the site's weather, which solar inverters without a set
        sunlight follow; arguments left out keep their values, and a microgrid
        without weather gets one from the simulator's defaults.

        `sunrise` and `sunset` are UTC times of day in whole minutes. Each
        random cloud's depth, duration and ramp are picked from their (low,
        high) ranges, one cloud every `cloud_mean_gap` on average
        (``timedelta(0)``: none). A `seed` makes the random clouds the same in
        every run; it restarts them, and the clouds in the sky go. A `seed`
        outside 0 to 2**64 - 1 raises `ValueError`.
        """
        label = self._label("set_weather")
        if is_int_like(seed) and not 0 <= seed < 2**64:
            raise ValueError(f"{label}: seed must be 0 to 2**64 - 1, not {seed}")
        body = wire_body(
            label,
            {
                "peak": (peak, Percentage),
                "cloud_mean_gap": (cloud_mean_gap, timedelta),
                "seed": (seed, int),
            },
        )
        for name, value in (("sunrise", sunrise), ("sunset", sunset)):
            if value is not None:
                body[name] = _hhmm(f"{label} {name}", value)
        for name, value, kind, suffix in (
            ("cloud_depth", cloud_depth, Percentage, "_pct"),
            ("cloud_duration", cloud_duration, timedelta, "_s"),
            ("cloud_ramp", cloud_ramp, timedelta, "_s"),
        ):
            pair = _pair(f"{label} {name}", value, kind)
            if pair is not None:
                body[name + suffix] = pair
        await self._post("weather", body, "set_weather()")

    async def pass_cloud(
        self, depth: Percentage, duration: timedelta, ramp: timedelta | None = None
    ) -> None:
        """Pass one cloud over the site now: it takes `depth` off the sunlight
        for `duration`, growing and fading over `ramp`. Raises `Rejected` on a
        microgrid without weather."""
        label = self._label("pass_cloud")
        cloud = wire_body(
            label,
            {"depth": (depth, Percentage), "duration": (duration, timedelta)},
        )
        cloud |= wire_body(label, {"ramp": (ramp, timedelta)})
        await self._post("weather", {"pass_cloud": cloud}, f"pass_cloud({depth})")

    async def clear_weather(self) -> None:
        """Remove the site's weather; a later `set_weather` starts from the
        defaults again. A rebuild keeps the weather, so this is the way to drop
        it."""
        await self._session.delete(
            self._session.microgrid_path("weather"),
            call=f"{self._label('clear_weather')}()",
        )

    def _key_id(self, key: object) -> int | None:
        if isinstance(key, ComponentSpec):
            return self._spec._ids.get(key)
        if is_int_like(key):
            number = int(key)
            return number if number in self._by_id else None
        raise TypeError(
            f"look a component up by its spec or its int id, not {type_and_value(key)}"
        )

    @overload
    def __getitem__(self, key: ComponentSpec[L]) -> L: ...

    @overload
    def __getitem__(self, key: int) -> Component: ...

    def __getitem__(self, key: object) -> Any:
        cid = self._key_id(key)
        if cid is None:
            ids = fmt_ids(self._by_id)
            if isinstance(key, ComponentSpec):
                message = (
                    f"no component {key!r} in this microgrid (specs are matched by "
                    f"object identity); component ids: {ids}"
                )
            else:
                shown = repr_or_none(int(cast("int", key))) or "with that id"
                message = f"no component {shown} in this microgrid; component ids: {ids}"
            raise UnknownComponent(message, key=key, valid_ids=sorted(self._by_id))
        live = self._live.get(cid)
        if live is None:
            spec = self._by_id[cid]
            live = LIVE_CLASS[type(spec)](self._session, cid)
            self._live[cid] = live
        return live

    def __contains__(self, key: object) -> bool:
        if not isinstance(key, ComponentSpec) and not is_int_like(key):
            return False
        return self._key_id(key) is not None

    __repr__ = _microgrid_repr
