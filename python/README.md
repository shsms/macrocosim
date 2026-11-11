# macrocosim (Python)

An integration-testing client for
[macrocosim](https://github.com/shsms/macrocosim), a microgrid simulator that
serves the gRPC Microgrid API. A test describes a microgrid, starts a private
simulator for it, points the app you are testing at the simulator's gRPC
address, then changes the simulated world (loads, state of charge, sunlight,
faults) and waits for what the simulator measures.

The app you are testing commands the inverters; the test only changes the world
around it and observes.

The same client runs site scripts: plain programs that connect to a running
simulator, declare their microgrids and animate them with these objects (see
[Scripts](#scripts)).

## Install

The wheel bundles the `macrocosim` and `macroctl` binaries, so there is nothing
else to fetch:

```sh
pip install macrocosim pytest pytest-asyncio
```

`launch` finds the bundled simulator in the interpreter's scripts directory,
then on `PATH`. Set `MACROCOSIM_BIN` to use another build, such as a local
`cargo build`.

## A first test

`test_first.py`:

```python
"""A first test: set a load and expect the grid connection point to see it."""

from datetime import timedelta

from frequenz.quantities import Power

import macrocosim as mc


async def test_grid_power_follows_the_load() -> None:
    load = mc.make_meter()
    spec = mc.make_microgrid(topology=mc.make_grid_connection_point(successors=[load]))

    async with mc.launch(spec) as microgrid:
        await microgrid[load].power.set(Power.from_kilowatts(20))
        await microgrid.grid_power.expect(
            mc.near(Power.from_kilowatts(20), tol=Power.from_kilowatts(1)),
            timeout=timedelta(seconds=30),
        )
```

The test describes a microgrid, starts a simulator for it, changes the load and
waits for the grid connection point to measure it. Leaving the `async with`
block stops the simulator. A real test also starts the app you are testing,
pointed at `microgrid.grpc_url`.

The test needs pytest-asyncio's auto mode, in `pyproject.toml`. Without it,
pytest fails every `async def` test with "async def functions are not natively
supported". The second line avoids pytest-asyncio's deprecation warning:

```toml
[tool.pytest.ini_options]
asyncio_mode = "auto"
asyncio_default_fixture_loop_scope = "function"
```

The file is
[`examples/testing/test_first.py`](../examples/testing/test_first.py), which CI
runs.

## Topology specs

Each `make_*` constructor returns an inert, frozen spec: `make_microgrid`,
`make_grid_connection_point`, `make_meter`, `make_battery`,
`make_battery_inverter`, `make_solar_inverter`, `make_chp`, `make_ev_charger`,
`make_steam_boiler`, `make_wind_turbine`, `make_power_transformer` and
`make_breaker`. A field left out uses the simulator's default. Values are typed:
[frequenz-quantities](https://pypi.org/project/frequenz-quantities/) (`Power`,
`Energy`, `Percentage`, ...), `datetime.timedelta`, and this package's
`Pressure` (bar) and `MassFlow` (kg/s) for a steam boiler; a bare number where a
quantity belongs is a `TypeError`.

**Ids.** `id=` is optional on every component spec. No id means the test does
not care which number the component gets; give an explicit id to any component
the test refers to by number. Automatic ids may change between versions. Look
components up by the spec object itself: `microgrid[LOAD]`. A spec used under
two parents is one shared component.

**From a Lisp config.** The
[porting guide](https://github.com/shsms/macrocosim/blob/main/docs/python-scripts.md#porting-from-lisp)
covers whole site files. A hand-written `(make-meter :id 410 :power-w 5000.0)`
becomes `mc.make_meter(id=410, power=Power.from_watts(5000))`. To convert a
form:

*   Replace `-` with `_` in the keyword.
*   Drop the unit suffix.
*   Wrap the value in its quantity type.
*   Use enum members for symbols.
*   Use `True` and `False` for `t` and `nil`.
*   Turn `let*` bindings into Python variables.

There are two exceptions. `:ramp-rate-w-per-s` and
`:reactive-ramp-rate-var-per-s` keep their suffix and take plain floats.
`:topology` takes a grid-connection-point spec, not a lambda.

## Live components

`async with mc.launch(spec, env=..., state_dir=...) as microgrid:` yields an
`mc.Microgrid` with `id`, `grpc_url`, `ui_url`, `log_path`, the aggregates
`grid_power`, `pv_power`, `consumer_power`, `battery_power` and their `*_energy`
totals, and `microgrid[spec]` / `microgrid[id]`.

| Live class | Read and expect | `set` | `clear()` | Fault knobs (write-only) |
|---|---|---|---|---|
| `Meter` | `power`, `reactive_power` | both | both | `health`, `telemetry_mode`, `command_mode`, `operational_mode` |
| `Battery` | `soc` | `soc` | — | same |
| `BatteryInverter` | `power` | — | — | same |
| `SolarInverter` | `power` | `sunlight` (write-only) | `sunlight` | same |
| `EvCharger` | `power` | — | — | same; and `plug(preset, ...)`, `unplug()` |
| `SteamBoiler` | `power`, `pressure` | `pressure`, `demand` (write-only) | — | same |
| `GridConnectionPoint`, `Chp`, `WindTurbine`, `PowerTransformer`, `Breaker` | — | — | — | same |

Every signal has `read(timeout=5 s)` and `expect(matcher, timeout=10 s,
hold_for=None)`, with the matchers `near`, `between`, `at_most` and `at_least`:

*   `read` returns the first sample with a value that is newer than the newest
    sample the simulator had when the call started. It skips samples with no
    value. Right after a `set` it can still return the old value; use `expect`
    to wait for a change.
*   `expect` waits up to `timeout` for a new sample to match. With `hold_for`,
    the first matching sample starts the hold, and every sample for that long
    must match too. It returns `None`.

Signs are in
[names-and-units](https://github.com/shsms/macrocosim/blob/main/docs/names-and-units.md):
meter and battery inverter power are positive when consuming or charging, solar
power is negative while producing, `grid_power` is positive when importing.
Energy totals count from when the simulator's aggregates started, not from the
start of the test: read twice and subtract to measure an interval.

**What `clear()` means.** It does not restore the spec's value.
`Meter.power.clear()` makes the meter measure its children again (0 W for a leaf
meter); `reactive_power.clear()` also ends a power-factor setting;
`sunlight.clear()` makes the inverter follow the site's weather. Set power on
leaf (load) meters: on a meter with children it replaces their sum. A `soc` set
is a one-time jump; the physics carries on from the new value.

**Fault knobs.** Any `health` other than `OK` makes the simulator refuse the
app's commands to that component. An inverter that is not `OK` stops: its power
goes to 0. An inverter shares its power among its batteries that are `OK`, and
publishes 0 when none is. A meter keeps measuring.

`health=ERROR` also sets `command_mode` to `ERROR`; `STANDBY` leaves
`command_mode` as it is. `health=OK` sets `command_mode` back to `NORMAL` if the
component's operational mode accepts control, else to `ERROR`. So set
`command_mode` after `health`.

**Reads and telemetry faults.** Component reads (`microgrid[x].power`,
`.reactive_power`, `.soc`) come from the simulator's own 1 Hz sampler, so a
value can be up to one second old, and they do not show telemetry faults,
stream intervals or jitter. This is a current
limitation. Aggregates such as `microgrid.grid_power` are computed from the gRPC
telemetry the app receives, and they do show every fault. Health and command
faults change what the components do or what the app can command, so both kinds
of read show their effect.

`operational_mode` sets `telemetry_mode` to `NORMAL`, or `SILENT` without
telemetry, and `command_mode` to `NORMAL`, or `ERROR` without control or while
`health` is `ERROR`. Like every knob, it changes the running component, not the
spec.

**EV chargers.** `plug(preset, soc=..., target_soc=..., phases=...,
max_current=..., capacity=..., taper_start=..., taper_floor=...)` plugs a car
from the simulator's catalog in; the arguments change that preset's values.
`unplug()` takes it away.

**The microgrid's own knobs.** `set_voltage_per_phase(l1, l2, l3)`,
`set_sample_lag(lag)` (shifts the timestamps the app receives into the past),
`cancel_streams()`, `set_weather(sunrise=..., sunset=..., peak=...,
cloud_mean_gap=..., cloud_depth=(lo, hi), cloud_duration=(lo, hi),
cloud_ramp=(lo, hi), seed=...)`, `pass_cloud(depth, duration, ramp=None)` and
`clear_weather()`. Weather, voltage and sample lag stay until changed.

**The grid frequency** is one value for the whole simulator, shared by every
microgrid in it. `microgrid.frequency.set(f)` holds it at `f`, and
`clear()` hands it back to the frequency model, which
`set_frequency_model(nominal=..., mean_rev_rate=..., sigma=...)` changes.

**A category the topology lacks** reads 0: `pv_power` is 0 W on a site without
solar.

## Launch options and the log

*   `env={...}` is laid over the test's environment for the simulator process
    only, for example `{"FAKETIME": "+3600", "LD_PRELOAD": ...}`. Only
    `FAKETIME` offsets are supported, not speed factors.
*   `state_dir=` (absent or empty; created if absent) is where the simulator
    runs: `topology.json`, `endpoints.json` and `macrocosim.log`. It is kept, so
    pass pytest's `tmp_path` to keep the log of a failing test. Without it, a
    temporary directory is used and deleted, and error messages say so instead
    of naming a log.
*   `ready_timeout=` (20 s) bounds the start-up.

Leaving the block stops the simulator: the client waits up to 5 s for it to
exit, then kills it if it is still running.

## Fixtures

A fixture can take over launching and stopping, as long as the test still names
its spec. This one, in `conftest.py`, returns a function that launches a spec
and keeps each simulator's log under the test's `tmp_path`:

```python
"""A fixture that launches the spec a test hands it, and stops it afterwards."""

import contextlib
import itertools
from collections.abc import AsyncIterator, Awaitable, Callable
from pathlib import Path

import pytest

import macrocosim as mc

LaunchMicrogrid = Callable[[mc.MicrogridSpec], Awaitable[mc.Microgrid]]


@pytest.fixture
async def launch_microgrid(tmp_path: Path) -> AsyncIterator[LaunchMicrogrid]:
    """Return a function that launches a spec; stop every simulator it started
    when the test ends.

    Each simulator's log stays in tmp_path.
    """
    count = itertools.count(1)
    async with contextlib.AsyncExitStack() as stack:

        async def start(spec: mc.MicrogridSpec) -> mc.Microgrid:
            state_dir = tmp_path / f"macrocosim-{next(count)}"
            return await stack.enter_async_context(mc.launch(spec, state_dir=state_dir))

        yield start
```

A test asks for it and passes its spec:

```python
LaunchMicrogrid = Callable[[mc.MicrogridSpec], Awaitable[mc.Microgrid]]


async def test_grid_power_holds(launch_microgrid: LaunchMicrogrid) -> None:
    microgrid = await launch_microgrid(SPEC)
    await microgrid[LOAD].power.set(Power.from_kilowatts(7))
    ...
```

The simulators stop when the test ends. A fixture that also starts the app you
are testing is the natural next step; that part depends on the app. The full
example is [`examples/testing/test_demo.py`](../examples/testing/test_demo.py).

## Loads that change over time

Specs and setters take constants. Drive a changing load from a task:

```python
async def ramp(meter: mc.Meter) -> None:
    for kw in range(0, 21, 2):
        await meter.power.set(Power.from_kilowatts(kw))
        await asyncio.sleep(1)

task = asyncio.create_task(ramp(microgrid[LOAD]))
```

Each `set` is one HTTP request. One simulator supports 1600 writes per second
in total, across all its knobs and clients: the rate
[`tools/write-rate.py`](../tools/write-rate.py) measured, rounded down, with the
release binary writing ten meters from one task on a 4-core VM. Keep a loop
under 10 Hz per knob.

## Errors

| Error | Means |
|---|---|
| `SpecError` (a `ValueError`) | the client found a spec invalid; raised where it is built |
| `LaunchError` | the simulator could not start or get ready; carries the log tail |
| `ServerError` | the simulator's HTTP API failed, or answered something unexpected |
| `SimulatorExited` (a `ServerError`) | the simulator process is gone |
| `Rejected` | the simulator refused a set or a fault, with its reason |
| `UnknownComponent` (a `LookupError`) | no such component in this microgrid |
| `NoSample` | no usable sample: none in time, a stall, a gap or a null |
| `ExpectationFailed` (an `AssertionError`) | samples arrived and disagreed |
| `MicrogridClosed` | the microgrid was used after its `launch` or `connect` block exited |
| `ConnectionClosed` | the `Connection` was used after its `connect` block exited |

All derive from `MacrocosimError`. When a spec is built, the client checks
types, finite values, ids and a few other rules. A wrong type raises
`TypeError`. A value that fails the other checks raises `ValueError` or its
subclass `SpecError`.

The client does not check every value. The simulator refuses other invalid
values when it starts, for example a negative interval or a power factor outside
(0, 1]. `launch` then raises `LaunchError` with the log tail.

## Scripts

A script is a plain program. Start the simulator (`macrocosim`), then run the
script (`python plant.py`): it connects, declares its microgrids and animates
them.

```python
import asyncio

from frequenz.quantities import Power

import macrocosim as mc

LOAD = mc.make_meter(id=2)
MICROGRID = mc.make_microgrid(id=7, topology=mc.make_grid_connection_point(id=1, successors=[LOAD]))


async def main() -> None:
    async with mc.connect() as sim:
        plant = await sim.declare(MICROGRID)
        for kw in range(0, 21, 2):
            await plant[LOAD].power.set(Power.from_kilowatts(kw))
            await asyncio.sleep(1)


if __name__ == "__main__":
    asyncio.run(main())
```

`mc.connect(url=None)` yields a `Connection`; without a URL it uses
`$MACROCOSIM_URL`, else `http://127.0.0.1:8801`. `await sim.declare(spec)`
creates the microgrid, or updates it when its id is loaded: an unchanged spec
leaves it as it is, and a changed one rebuilds it, keeping the state of every
component that did not change. It returns the same `Microgrid` a test gets from
`launch`; `declare(a, b, ...)` returns a tuple. `await sim.unload(id)` unloads
one, and `await sim.microgrids()` lists what is loaded, by id. Declared
microgrids stay loaded after the script ends.

Each microgrid needs an explicit `id`, and a component whose state (state of
charge, a plugged car, boiler pressure) should survive an edit of the topology
needs one too. The
[guide](https://github.com/shsms/macrocosim/blob/main/docs/python-scripts.md)
covers re-runs, recording and porting from Lisp.

**Recording.** `async with microgrid.record(name, csv=False) as recording:`
records the block's metrics; `recording.event(kind, payload)` adds a named
event, and `recording.report()` returns a `RecordingReport` after the block.

**Specs in tests.** A test imports a script's specs and launches one: `from
plant import MICROGRID`, then `mc.launch(MICROGRID)`. Importing the script does
not run its `main`. `mc.load_topology(path)` reads a topology document (a
`--topology` file) back into `MicrogridSpec` objects.

## Interactive use

`python -m asyncio` gives a REPL where `await` works at the top level.
`mc.connect()` attaches to a simulator that is already running:

```python
>>> import macrocosim as mc
>>> from frequenz.quantities import Power
>>> async with mc.connect() as sim:
...     microgrids = await sim.microgrids()
...     await microgrids[7][2].power.set(Power.from_kilowatts(20))
```

`microgrids()` builds each microgrid from the simulator's copy of its spec, so
look components up by id. Leaving the block leaves the simulator running. This
is for one-off reads and writes: a write to a knob a script animates is
overwritten by the animation a moment later, so scenarios belong in the script.
To start a private simulator from the REPL instead:

```python
>>> cm = mc.launch(spec); microgrid = await cm.__aenter__()
>>> await microgrid.grid_power.read()
```

## Development

The package is [uv](https://docs.astral.sh/uv/)-native; the build backend is
maturin, so `uv sync` compiles the binaries from the repository's Rust crate
(needs the Rust toolchain, `protoc` and `git submodule update --init
--recursive`).

```sh
uv sync                       # venv from uv.lock; builds the binaries
uv run pytest                 # the suite; ruff and ty also via `uv run`
uv run pytest ../examples/testing
```

Point the tests at a local build instead of the wheel's binary with
`MACROCOSIM_BIN=../target/release/macrocosim`. The binary must be built from the
same version as the package: `launch` raises `LaunchError` naming both versions
when they differ.

## License

The Python package is [MIT](LICENSE-MIT). The wheel also bundles the
`macrocosim` and `macroctl` binaries, which are [GPL-3.0-only](LICENSE-GPL-3.0);
their source is the [macrocosim repository](https://github.com/shsms/macrocosim)
at the release's git tag: `v0.2.0` for wheel version 0.2.0, and for a
pre-release the semver form of the PEP 440 version, so `v0.2.0-alpha.1` for
0.2.0a1.
