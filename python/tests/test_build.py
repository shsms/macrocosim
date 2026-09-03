"""Emitter tests for the topology builder — pure, no binary required."""

from __future__ import annotations

import inspect
from datetime import time, timedelta

from frequenz.quantities import (
    ApparentPower,
    Current,
    Energy,
    Percentage,
    Power,
    ReactivePower,
    Voltage,
)

import macrocosim as mc
from macrocosim.build import Microgrid, battery, battery_inverter, grid, meter, raw


def test_nested_successors_emit_make_forms() -> None:
    mg = Microgrid(
        id=1,
        topology=grid(id=1, successors=[meter(id=2, power=Power.from_watts(7000))]),
    )
    lisp = mg.to_lisp()
    assert "(make-microgrid :id 1" in lisp
    assert "(make-grid-connection-point :id 1 :successors (list" in lisp
    assert "(make-meter :id 2 :power 7000.0)" in lisp


def test_ergonomic_kwargs_map_to_plist_keys() -> None:
    # rated (a Power pair) → two keys in watts (inverters carry rated bounds).
    inv = battery_inverter(
        id=3, rated=(Power.from_kilowatts(-5), Power.from_kilowatts(5))
    )
    inv_lisp = inv.to_lisp()
    assert ":rated-lower -5000.0" in inv_lisp
    assert ":rated-upper 5000.0" in inv_lisp
    # Energy → Wh; Percentage → percent; soc → initial-soc (battery conveniences).
    bat_lisp = battery(
        id=4,
        capacity=Energy.from_kilowatt_hours(100),
        initial_soc=Percentage.from_percent(50),
        rated=(Power.from_kilowatts(-30), Power.from_kilowatts(30)),
    ).to_lisp()
    assert ":capacity 100000.0" in bat_lisp
    assert ":initial-soc 50.0" in bat_lisp
    assert ":rated-lower -30000.0" in bat_lisp
    assert ":rated-upper 30000.0" in bat_lisp


def test_value_kinds_render_correctly() -> None:
    # symbol-valued key (enum), string-valued key, bool → t, and the
    # interval timedelta lands as whole server-side milliseconds.
    node = meter(
        id=2,
        name="main",
        hidden=True,
        health=mc.Health.ERROR,
        interval=timedelta(seconds=5),
    )
    lisp = node.to_lisp()
    assert ':name "main"' in lisp
    assert ":hidden t" in lisp
    assert ":health 'error" in lisp  # symbol, not a string
    assert ":interval 5000" in lisp  # milliseconds, no decimal


def test_meter_reactive_kwargs_render_correctly() -> None:
    # A constant VAr load renders as :reactive-power with the VAr value.
    lisp = meter(
        id=2, reactive_power=ReactivePower.from_kilo_volt_amperes_reactive(1.5)
    ).to_lisp()
    assert ":reactive-power 1500.0" in lisp

    # The power-factor form renders the ratio plus the leading flag.
    lisp = meter(id=3, power_factor=0.8, leading=True).to_lisp()
    assert ":power-factor 0.8" in lisp
    assert ":leading t" in lisp

    # leading defaults to absent, not `nil`, when not asked for.
    lisp = meter(id=4, power_factor=0.9).to_lisp()
    assert ":power-factor 0.9" in lisp
    assert ":leading" not in lisp


def test_live_signal_path_exposes_reactive_power() -> None:
    """`Meter.reactive_power` reads through the bound *async* Site.

    `_bind` is only ever called with `macrocosim.aio.Site`, so the read
    path needs the async twins — the sync ones alone leave the signal
    raising AttributeError on first use.
    """
    from macrocosim.aio._grpc import AsyncGrpcClient
    from macrocosim.aio._site import Site as AsyncSite
    from macrocosim.runtime import Site as SyncSite

    for holder in (AsyncSite, AsyncGrpcClient, SyncSite):
        assert hasattr(holder, "active_power"), holder
        assert hasattr(holder, "reactive_power"), holder

    assert inspect.iscoroutinefunction(AsyncSite.reactive_power)
    assert inspect.iscoroutinefunction(AsyncGrpcClient.reactive_power)


def test_to_lisp_atom_converts_typed_values() -> None:
    assert mc.to_lisp_atom(Power.from_kilowatts(2)) == "2000.0"
    assert mc.to_lisp_atom(Energy.from_kilowatt_hours(1)) == "1000.0"
    assert mc.to_lisp_atom(Percentage.from_percent(50)) == "50.0"
    assert mc.to_lisp_atom(timedelta(seconds=30)) == "30.0"
    assert mc.to_lisp_atom(time(12, 0)) == '"12:00:00"'
    assert mc.to_lisp_atom(mc.Health.ERROR) == "'error"
    assert mc.to_lisp_atom(True) == "t"
    assert mc.to_lisp_atom(5) == "5"


def test_raw_splices_literal_lisp() -> None:
    node = meter(id=2, power=raw("(lambda () (+ 1000.0 (random 500)))"))
    assert ":power (lambda () (+ 1000.0 (random 500)))" in node.to_lisp()


def test_branching_is_a_successors_list() -> None:
    mg = Microgrid(
        id=1,
        topology=grid(
            id=1,
            successors=[
                meter(id=2, successors=[battery_inverter(id=3)]),
                meter(id=5, power=Power.from_watts(-2000)),
            ],
        ),
    )
    lisp = mg.to_lisp()
    assert "(make-battery-inverter :id 3)" in lisp
    assert "(make-meter :id 5 :power -2000.0)" in lisp


def test_public_constructors_exported() -> None:
    for name in (
        "grid",
        "meter",
        "battery_inverter",
        "solar_inverter",
        "battery",
        "ev_charger",
        "chp",
        "steam_boiler",
        "raw",
        "Microgrid",
    ):
        assert hasattr(mc, name), name


def test_builders_cover_every_server_arg() -> None:
    # One call per builder exercising the newly named parameters; each
    # keyword must land under the server's exact plist key and unit.
    g = mc.grid(
        id=1,
        rated_fuse_current=Current.from_amperes(63),
        stream_jitter_pct=Percentage.from_percent(5),
    ).to_lisp()
    assert ":rated-fuse-current 63" in g
    assert ":stream-jitter-pct 5.0" in g

    inv = mc.battery_inverter(
        id=3,
        interval=timedelta(milliseconds=500),
        command_delay=timedelta(milliseconds=250),
        ramp_rate=1000.0,
        reactive_pf_limit=0.9,
        reactive_apparent_va=ApparentPower.from_volt_amperes(10000),
        reactive_command_delay=timedelta(milliseconds=100),
        reactive_ramp_rate=2000.0,
    ).to_lisp()
    assert ":interval 500" in inv
    assert ":command-delay-ms 250" in inv
    assert ":ramp-rate 1000.0" in inv
    assert ":reactive-pf-limit 0.9" in inv
    assert ":reactive-apparent-va 10000.0" in inv
    assert ":reactive-command-delay-ms 100" in inv
    assert ":reactive-ramp-rate 2000.0" in inv

    bat = mc.battery(
        id=4,
        soc_lower=Percentage.from_percent(10),
        soc_upper=Percentage.from_percent(90),
        soc_protect_margin=Percentage.from_percent(5),
        voltage=Voltage.from_volts(800),
    ).to_lisp()
    assert ":soc-lower 10.0" in bat
    assert ":soc-upper 90.0" in bat
    assert ":soc-protect-margin 5.0" in bat
    assert ":voltage 800.0" in bat

    ev = mc.ev_charger(
        id=6,
        capacity=Energy.from_kilowatt_hours(75),
        initial_soc=Percentage.from_percent(30),
        command_delay=timedelta(milliseconds=200),
        ramp_rate=500.0,
    ).to_lisp()
    assert ":capacity 75000.0" in ev
    assert ":initial-soc 30.0" in ev
    assert ":command-delay-ms 200" in ev

    # make-chp takes no rated bounds; the builder no longer offers them.
    import inspect

    assert "rated" not in inspect.signature(mc.chp).parameters
    chp_lisp = mc.chp(id=7, stream_jitter_pct=Percentage.from_percent(2)).to_lisp()
    assert ":stream-jitter-pct 2.0" in chp_lisp


def test_steam_boiler_renders_rated_and_physics_kwargs() -> None:
    c = mc.steam_boiler(
        id=7,
        rated=(Power.from_watts(0), Power.from_watts(100_000)),
        target_bar=6.0,
        max_bar=9.0,
        demand_kg_h=40.0,
    )
    text = c.to_lisp()
    assert "make-steam-boiler" in text
    assert ":rated-upper 100000.0" in text
    assert ":target-bar 6.0" in text
    assert ":max-bar 9.0" in text
    assert ":demand 40.0" in text
