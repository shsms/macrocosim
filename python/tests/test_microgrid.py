"""Microgrid: addresses, lookups and the tree repr (no simulator needed)."""

from __future__ import annotations

from enum import IntEnum
from typing import Any

import httpx
import pytest

import macrocosim._spec as s
from macrocosim._components import Battery, BatteryInverter, Meter
from macrocosim._errors import UnknownComponent
from macrocosim._microgrid import Microgrid
from macrocosim._process import Endpoints
from macrocosim._session import _Session

pytestmark = pytest.mark.filterwarnings("error")


def _mg(spec: s.MicrogridSpec, grpc_addr: str = "127.0.0.1:41235") -> Microgrid:
    session = _Session(
        "http://sim",
        7,
        transport=httpx.MockTransport(lambda r: httpx.Response(204)),
        exit_probe=lambda: None,
    )
    endpoints = Endpoints(ui="127.0.0.1:8801", microgrid_id=7, grpc_addr=grpc_addr)
    return Microgrid._create(session, spec, endpoints)


def test_grpc_url_prefixes_scheme() -> None:
    mg = _mg(
        s.make_microgrid(topology=s.make_grid_connection_point(id=1)), "10.0.0.1:61000"
    )
    assert mg.grpc_url == "grpc://10.0.0.1:61000"
    assert mg.ui_url == "http://127.0.0.1:8801"
    assert mg.id == 7


def test_lookups_by_spec_id_and_intenum_share_one_object() -> None:
    class Ids(IntEnum):
        LOAD = 104

    load = s.make_meter(id=104)
    mg = _mg(s.make_microgrid(topology=s.make_grid_connection_point(successors=[load])))
    live = mg[load]
    assert isinstance(live, Meter)
    assert mg[104] is live and mg[Ids.LOAD] is live
    assert live.id == 104
    assert load in mg and 104 in mg and 5 not in mg and "x" not in mg


def test_an_automatic_id_is_reported() -> None:
    bat = s.make_battery()
    mg = _mg(
        s.make_microgrid(
            topology=s.make_grid_connection_point(
                successors=[s.make_battery_inverter(id=2, successors=[bat])]
            )
        )
    )
    assert isinstance(mg[bat], Battery)
    assert mg[bat].id not in (2, None)


def test_unknown_id_and_foreign_spec() -> None:
    mg = _mg(s.make_microgrid(topology=s.make_grid_connection_point(id=100)))
    with pytest.raises(UnknownComponent) as info:
        mg[5]
    assert str(info.value) == "no component 5 in this microgrid; component ids: 100"
    assert info.value.valid_ids == (100,)
    stranger = s.make_meter(id=100)
    with pytest.raises(UnknownComponent) as info:
        mg[stranger]
    assert str(info.value) == (
        "no component MeterSpec(id=100) in this microgrid (specs are matched by "
        "object identity); component ids: 100"
    )
    assert isinstance(info.value, LookupError)


@pytest.mark.parametrize("key", [True, "100", 100.0])
def test_wrong_key_types_raise_type_error(key: Any) -> None:
    mg = _mg(s.make_microgrid(topology=s.make_grid_connection_point(id=100)))
    with pytest.raises(TypeError, match="look a component up by its spec or its int id"):
        mg[key]


def test_a_microgrid_spec_is_not_a_key() -> None:
    spec = s.make_microgrid(topology=s.make_grid_connection_point(id=100))
    mg = _mg(spec)
    with pytest.raises(TypeError, match="not MicrogridSpec"):
        mg[spec]  # type: ignore[call-overload]


def test_users_cannot_construct_a_microgrid() -> None:
    with pytest.raises(TypeError):
        Microgrid(id=1, topology=None)  # type: ignore[call-arg]
    with pytest.raises(TypeError, match="built by macrocosim.launch"):
        Microgrid(1)


def test_repr_prints_the_tree() -> None:
    inv = s.make_battery_inverter(successors=[s.make_battery()])
    mg = _mg(
        s.make_microgrid(
            id=7,
            topology=s.make_grid_connection_point(
                successors=[
                    s.make_meter(successors=[inv]),
                    s.make_meter(successors=[inv]),
                ]
            ),
        )
    )
    assert repr(mg) == (
        "Microgrid 7 at grpc://127.0.0.1:41235\n"
        "└─ GridConnectionPoint 1004 (auto)\n"
        "   ├─ Meter 1002 (auto)\n"
        "   │  └─ BatteryInverter 1001 (auto)\n"
        "   │     └─ Battery 1000 (auto)\n"
        "   └─ Meter 1003 (auto)\n"
        "      └─ BatteryInverter 1001 (shared, see above)"
    )
    assert isinstance(mg[inv], BatteryInverter)


@pytest.mark.parametrize(
    ("name", "stream"),
    [
        ("grid_power", "grid_power"),
        ("pv_power", "pv_power"),
        ("consumer_power", "consumer_power"),
        ("battery_power", "battery_pool_power"),
        ("grid_energy", "grid_energy"),
        ("pv_energy", "pv_energy"),
        ("consumer_energy", "consumer_energy"),
        ("battery_energy", "battery_pool_energy"),
    ],
)
def test_each_aggregate_reads_its_stream(name: str, stream: str) -> None:
    mg = _mg(s.make_microgrid(topology=s.make_grid_connection_point()))
    source = getattr(mg, name)._source
    assert source.describe() == f"microgrid.{name}"
    assert source._def.stream == stream
