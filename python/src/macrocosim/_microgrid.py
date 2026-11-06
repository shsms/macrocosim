"""The live microgrid that `launch` yields.

Internal; imports `_components`, `_signals`, `_spec`, `_errors`, `_units`, and
`_session` and `_process` for typing only.
"""

from __future__ import annotations

from pathlib import Path
from typing import TYPE_CHECKING, Any, TypeVar, cast, overload

from frequenz.quantities import Energy, Power

from ._components import LIVE_CLASS, Component
from ._errors import UnknownComponent, fmt_ids
from ._signals import AGGREGATES, Signal, _AggregateSource
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
        return self

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

    def _key_id(self, key: object) -> int | None:
        if isinstance(key, ComponentSpec):
            return self._spec._ids.get(key)
        if is_int_like(key):
            number = int(cast("int", key))
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
