"""Site endpoint tests — address helpers, no process launched."""

from __future__ import annotations

import pytest
from frequenz.quantities import Energy

import switchyard as sw
from switchyard.handles import MicrogridExpect


def _site() -> sw.Site:
    return sw.connect(
        ui="127.0.0.1:8080",
        microgrids={
            1: sw.MicrogridEndpoint(id=1, name="a", grpc="10.0.0.1:61000"),
            2: sw.MicrogridEndpoint(id=2, name="b", grpc="10.0.0.2:61000"),
        },
    )


def test_grpc_is_first_microgrid_host_port() -> None:
    assert _site().grpc == "10.0.0.1:61000"


def test_grpc_url_prefixes_scheme() -> None:
    assert _site().grpc_url == "grpc://10.0.0.1:61000"


def test_microgrid_grpc_url_selects_by_id() -> None:
    site = _site()
    assert site.microgrid_grpc_url(2) == "grpc://10.0.0.2:61000"


class _EnergyStub:
    """A stand-in site returning a fixed grid energy."""

    def __init__(self, wh: float) -> None:
        self._e = Energy.from_watt_hours(wh)

    def grid_energy(self, _mg: int | None = None) -> Energy:
        return self._e


def test_expect_grid_energy_passes_within_matcher() -> None:
    expect = MicrogridExpect(_EnergyStub(66.0), None)  # type: ignore[arg-type]
    assert expect.grid_energy(max=Energy.from_watt_hours(100)) == (
        Energy.from_watt_hours(66.0)
    )


def test_expect_grid_energy_raises_when_over_cap() -> None:
    expect = MicrogridExpect(_EnergyStub(150.0), None)  # type: ignore[arg-type]
    with pytest.raises(AssertionError):
        expect.grid_energy(max=Energy.from_watt_hours(100))


def test_energy_metric_renders_to_lisp_symbol() -> None:
    assert sw.to_lisp_atom(sw.Metric.ENERGY) == "'energy"
