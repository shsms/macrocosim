"""Id assignment when a microgrid spec is built."""

from __future__ import annotations

from enum import IntEnum

import pytest

import macrocosim._spec as s
from macrocosim._errors import SpecError

pytestmark = pytest.mark.filterwarnings("error")


def _ids(mg: s.MicrogridSpec) -> list[int]:
    return [mg._ids[c] for c in mg._order]


def test_all_explicit_ids_are_kept() -> None:
    bat = s.make_battery(id=3)
    inv = s.make_battery_inverter(id=2, successors=[bat])
    gcp = s.make_grid_connection_point(id=1, successors=[inv])
    mg = s.make_microgrid(id=7, topology=gcp)
    assert mg._order == (bat, inv, gcp)
    assert _ids(mg) == [3, 2, 1]


def test_all_automatic_ids_are_distinct_and_children_come_first() -> None:
    bat = s.make_battery()
    inv = s.make_battery_inverter(successors=[bat])
    load = s.make_meter()
    gcp = s.make_grid_connection_point(successors=[inv, load])
    mg = s.make_microgrid(topology=gcp)
    assert mg._order == (bat, inv, load, gcp)
    assert len(set(_ids(mg))) == 4
    assert bat.id is None  # the spec itself is never changed


@pytest.mark.parametrize("explicit", [1, 1000, 1001, 5000])
def test_an_automatic_id_never_takes_an_explicit_one(explicit: int) -> None:
    autos = [s.make_meter() for _ in range(3)]
    pinned = s.make_meter(id=explicit)
    gcp = s.make_grid_connection_point(successors=[*autos, pinned])
    mg = s.make_microgrid(topology=gcp)
    ids = _ids(mg)
    assert len(set(ids)) == len(ids)
    assert mg._ids[pinned] == explicit
    assert [mg._ids[a] for a in autos].count(explicit) == 0


def test_a_shared_spec_gets_one_id() -> None:
    inv = s.make_battery_inverter(successors=[s.make_battery()])
    a = s.make_meter(successors=[inv])
    b = s.make_meter(successors=[inv])
    mg = s.make_microgrid(topology=s.make_grid_connection_point(successors=[a, b]))
    assert sum(c is inv for c in mg._order) == 1
    assert len(mg._order) == 5


def test_duplicate_explicit_ids_name_both_specs() -> None:
    a = s.make_meter(id=4, name="a")
    b = s.make_meter(id=4, name="b")
    with pytest.raises(SpecError, match=r"name='a'.*name='b'.*both have id 4"):
        s.make_microgrid(topology=s.make_grid_connection_point(successors=[a, b]))


def test_the_microgrid_id_is_not_checked_against_component_ids() -> None:
    gcp = s.make_grid_connection_point(id=1)
    mg = s.make_microgrid(id=1, topology=gcp)
    assert mg._ids[gcp] == 1


def test_intenum_ids_count_as_their_number() -> None:
    class Ids(IntEnum):
        GRID = 1
        LOAD = 2

    load = s.make_meter(id=Ids.LOAD)
    mg = s.make_microgrid(
        id=Ids.GRID, topology=s.make_grid_connection_point(id=Ids.GRID, successors=[load])
    )
    assert mg._ids[load] == 2
    assert type(mg._ids[load]) is int
