# Python scripts

A macrocosim script is an ordinary Python program that declares microgrids on a
running simulator and animates them. Start the simulator once, then run the
script with `python plant.py`, as often as you like: each run updates the
microgrids it declares, and they stay loaded after it ends. The script uses the
same `macrocosim` client as a pytest test, so code moves freely between the two.

This page describes the script shape, what a re-run keeps, recording, and how to
port a Lisp site file. The client itself is described in the [Python
README](../python/README.md). Short scripts, one feature each, are in
[`examples/`](../examples/README.md).

## Requirements

Python 3.11 or later, with the `macrocosim` package at exactly the binary's
version. The `macrocosim` wheel ships the binary, so a venv with the wheel has
both:

```sh
pip install macrocosim
macrocosim                # the simulator; leave it running
python plant.py           # in another terminal
```

A declaration carries the package's version, and the simulator refuses one of
another version with a message that names both.

The simulator itself runs Python only to export a microgrid as a `.py` module
("Save as .py" in the UI). It looks for the interpreter the first time a
microgrid is exported, and the first match wins:

1.  `--python PATH`;
2.  `$MACROCOSIM_PYTHON`;
3.  a `python` or `python3` next to the binary, which is the wheel's layout;
4.  `python3`, then `python`, on `PATH`.

It checks that one interpreter and does not try the next one, so a misconfigured
setup is reported instead of hidden. When the check fails, the export answers
503 with:

```text
macrocosim needs Python 3.11 or later with the macrocosim package, version
<version>. Found: <path> (<what failed>). Install it, or pass --python.
```

A failed check runs again on the next export, so installing the package fixes
the export without restarting the simulator. A check that passed is kept until
the simulator stops.

## The script file

```python
import asyncio
import math
from datetime import timedelta

from frequenz.quantities import Energy, Percentage, Power

import macrocosim as mc

load = mc.make_meter(id=2)
battery = mc.make_battery(
    id=4,
    capacity=Energy.from_kilowatt_hours(100),
    initial_soc=Percentage.from_percent(50),
)
inverter = mc.make_battery_inverter(
    id=3,
    rated_lower=Power.from_kilowatts(-50),
    rated_upper=Power.from_kilowatts(50),
    successors=[battery],
)
MICROGRID = mc.make_microgrid(
    id=7,
    topology=mc.make_grid_connection_point(id=1, successors=[load, inverter]),
)


async def first_hour(plant: mc.Microgrid) -> None:
    async with plant.record("first hour", csv=True):
        for minute in range(60):
            kw = 20 + 10 * math.sin(minute / 60 * 2 * math.pi)
            await plant[load].power.set(Power.from_kilowatts(kw))
            await asyncio.sleep(60)
    await plant.grid_power.expect(
        mc.at_most(Power.from_kilowatts(40)), timeout=timedelta(seconds=30)
    )


async def main() -> None:
    async with mc.connect() as sim:
        plant = await sim.declare(MICROGRID)
        await first_hour(plant)


if __name__ == "__main__":
    asyncio.run(main())
```

*   **`mc.connect(url=None)`** attaches to the simulator. Without a URL it uses
    `$MACROCOSIM_URL`, else `http://127.0.0.1:8801`, the simulator's default UI
    address. Leaving the block closes the connection; the simulator and the
    declared microgrids keep running.
*   **`await sim.declare(spec)`** creates the microgrid when its id is not
    loaded, and otherwise updates it (see [What a re-run
    keeps](#what-a-re-run-keeps)). It returns the same `Microgrid` a test gets
    from `launch`, so `plant[load]` finds a component by its spec object, and
    every knob, signal and `expect` works as in a test. `declare(a, b, ...)`
    declares several microgrids in one request and returns a tuple. Every
    microgrid spec needs an explicit `id`, so that a re-run updates the same
    microgrid.
*   **`await sim.unload(id)`** unloads a microgrid: its gRPC server stops and
    its ids are free again.
*   **`await sim.microgrids()`** returns every microgrid loaded now, by id,
    built from the simulator's copy of their specs, so look their components up
    by id (`microgrids[7][2]`).

A refused declaration (an id or a port held by a microgrid the declaration does
not list, another package version) raises `Rejected` with the simulator's
reason; an invalid spec raises `ServerError`.

The script's own directory comes first on the import path, as for any Python
program, so it can import helper modules beside it.

The script's output stays in its own terminal. A script that stops, crashes or
is killed leaves its microgrids loaded and serving gRPC, with every knob at the
last value it wrote; nothing in the UI shows that it stopped. Processes the
script started are its own to stop.

The binary's flags apply: `--state-dir`, `--ui-port`, `--ephemeral-ports`,
`--topology`, which boots the microgrids of a topology document, and the
enterprise settings `--enterprise-id`, `--assets-addr`, `--dispatch-addr`,
`--request-lifetime` and `--augment-lifetime`. A script can declare onto a
`--topology` microgrid like any other.

## What a re-run keeps

A declaration works per microgrid id. There is no owner: any script, and the UI,
may update any microgrid.

*   **A new id** is created.
*   **The same spec as the loaded one** changes nothing: no rebuild, so the app
    under test keeps its gRPC streams and setpoints, and every knob keeps the
    value the last run left, health and modes included. Reset what the run needs
    at its start, or unload the microgrid first.
*   **A changed spec** rebuilds the microgrid in place, as a UI edit does: its
    gRPC server, physics and UI keep running, and each component keeps its state
    when the new spec has a component with the same id and an equal spec. The
    name, health and modes don't count: changing only those keeps the state too.

| State | Same id, same spec | Changed or new component | Microgrid |
|---|---|---|---|
| Battery SoC, connected car, boiler pressure | kept | from the spec | |
| History and setpoint log (UI charts) | kept | cleared | |
| Energy totals | kept | start at zero | |
| Knob values (health, modes, meter power, sunlight, reactive source, steam demand) and names | from the spec | from the spec | |
| Gateway commands, request lifetimes, augmentations | cleared | cleared | |
| Ramps and setpoints | restart from zero | start from zero | |
| Telemetry streams | cancelled; a client opens them again | cancelled | |
| A running recording | | | ended (see [Recording](#recording)) |
| Weather, grid voltage, sample lag | | | kept until changed or cleared |
| Frequency | | | one value for the whole simulator; not touched |

The table is for a rebuild; a UI edit of the microgrid's structure and an undo
rebuild it the same way. A rebuild also puts the replaced spec on the
microgrid's undo stack, so Undo in the UI goes back to the previous run's spec.

A declaration also restarts a microgrid's gRPC server when it failed to start
(its port was taken, say), even when the spec did not change.

**Carry follows ids.** A component without an `id` gets an automatic one, from
1000 up, skipping explicit ids. Within one `connect` block the numbering
continues across `declare` calls, so one script's microgrids never share an
automatic id, and a microgrid declared again is numbered from the same start.
The automatic ids depend on the order of the `declare` calls and on the other
components and explicit ids, so adding, removing or moving a component can
renumber them. A component whose id changed does not keep its own state: it
takes the state of the component that held its new id in the same microgrid
before, if that one's spec was equal, and starts fresh otherwise; an id that
moved from another microgrid carries nothing.

Separate `declare` calls see only what came before them, so their ids can
collide:

*   On the next run, a microgrid whose automatic ids changed (it grew, was added
    or moved earlier, or one of its explicit ids changed or went) can take ids
    that a microgrid declared after it still holds.
*   On the next run, a call after a microgrid that is no longer declared, but is
    still loaded, can take that microgrid's ids.
*   On any run, an earlier call can take an explicit id that a later call gives.

The declaration that finds an id already held is refused, naming the microgrid
that holds it. To avoid this, declare all the script's microgrids in one call,
`declare(a, b, c)`, where ids may move between them, and unload a microgrid you
no longer declare; or give their components explicit ids.

Give an explicit id to every component whose state should survive an edit of the
topology, and to every component the app under test addresses: batteries, EV
chargers and steam boilers above all. The examples give every component one.

Two scripts that both leave ids out number from 1000 each, so the second one's
declaration is refused, naming the microgrid that holds the ids: give the
components explicit ids.

**Weather, grid voltage and sample lag** have no spec field, so a rebuild keeps
them. A script that stops calling `set_weather` keeps the old weather until it
calls `clear_weather()` or the simulator restarts; the same holds for
`set_voltage_per_phase` and `set_sample_lag`.

**Renaming or removing a microgrid.** Nothing is unloaded on its own. A
microgrid the script stops declaring, or declares under another id, stays loaded
with its component ids and gRPC port, so the renamed declaration is refused with
a message that names the old microgrid and how to unload it: `await
sim.unload(2200)`, or `DELETE /api/mg/2200`.

Component ids are unique across the simulator. A UI-made component takes an id
of 5000 or more, so it does not meet a script's automatic ids.

**Two runs at once.** A run started while the previous one still runs (a second
terminal, a background job) does not stop it: both write to the same microgrid's
knobs, and nothing reports it. Stop the old run first. To re-run the script on
every save, use a watcher that stops the old run before it starts the new one:

```sh
watchexec -r -e py -- python plant.py
```

A server restart loses every declared microgrid; run the scripts again, or the
files "Save as .py" exported.

## Animation and scenarios

Specs and knobs take constants. A script makes them change over time with
ordinary asyncio: a loop of `set` calls and `asyncio.sleep`, one task per
loop.

```python
async def consumer_load(plant: mc.Microgrid) -> None:
    while True:
        kw = 17.5 + 12.5 * math.sin(2 * math.pi * (time.time() % 900) / 900)
        await plant[CONSUMER].power.set(Power.from_kilowatts(kw))
        await asyncio.sleep(1)
```

A scenario is an async function that takes the microgrid. Each knob holds the
last value written, by any client, so a scenario that drives a knob the
animation also writes pauses that animation itself, for example with a flag or
an `asyncio.Event` that the animation loop checks. A scenario puts back every
knob it changed, in a `finally`:

```python
async def off_grid_island(plant: mc.Microgrid) -> mc.RecordingReport:
    grid = plant[GRID]
    try:
        async with plant.record("off-grid-island") as recording:
            await asyncio.sleep(60)
            await grid.health.set(mc.Health.ERROR)
            await asyncio.sleep(60)
    finally:
        await grid.health.set(mc.Health.OK)
    return recording.report()
```

Scenarios belong in the script: run one from `main`, awaited beside the
animation, and run the script again. Only there can it pause the animation,
which runs in the script's process.

## Recording

`Microgrid.record(name, *, csv=False)` is an async context manager that
records the microgrid's metrics for its block: peak grid power, battery
charge and discharge, PV energy, SoC statistics and 15-minute averages of the
grid power. The UI's Report panel shows the recording while it runs.

```python
async with plant.record("evening peak", csv=True) as recording:
    await recording.event("spike", "consumer to 25 kW")
    ...
report = recording.report()
```

*   `recording.event(kind, payload)` adds a named event, with any JSON value
    as its payload, and returns its id.
*   `recording.report()` returns the `RecordingReport` once the block has
    exited.
*   With `csv=True`, every component's telemetry goes to CSV files in
    `recordings/<microgrid id>/<name>-<UTC time>/` under the state directory.
*   A microgrid has one recording at a time: a second `record()` on it while
    one runs raises `RuntimeError`, and the simulator refuses a start
    (`Rejected`) while another client records it.

**When a rebuild ends it.** A UI edit, an undo or a declaration with a changed
spec rebuilds the microgrid and ends its recording, keeping the report up to the
rebuild. The block goes on, unrecorded:

*   `event()` logs a warning, drops the event and returns `None`;
*   leaving the block logs a warning instead of raising;
*   `report()` returns the report up to the rebuild, with
    `ended_by == "rebuild"` (a normal stop gives `"stop"`).

When the microgrid is unloaded during the block, leaving it logs a warning
and `report()` raises `RuntimeError`, since there is no report.

## Reads and writes

*   **Writes.** Each `set` is one HTTP request. Keep animation loops under
    10 Hz per knob.
*   **Reads.** `read()` and `expect` see the simulator's 1 Hz history, so a
    value can be up to one second old; see "Reads and telemetry faults" in
    the [Python README](../python/README.md#live-components).

## Attaching a terminal

`connect` works in `python -m asyncio`, where `await` works at the top level:

```python
from frequenz.quantities import Power

import macrocosim as mc

async with mc.connect() as sim:
    plant = (await sim.microgrids())[7]
    await plant[2].power.set(Power.from_kilowatts(20))
```

A terminal is for one-off reads and writes. A write stands until the next write
to the same knob, so a write to a knob a script animates is overwritten by the
animation a moment later. Scenarios belong in the script (see [Animation and
scenarios](#animation-and-scenarios)).

## Specs in tests

A test imports a script's specs and launches them with `launch`, which starts a
private simulator for one microgrid on free ports and stops it when the block
ends:

```python
from plant import MICROGRID


async def test_the_plant_holds_its_peak() -> None:
    async with mc.launch(MICROGRID) as plant:
        ...
```

The script's directory must be on the test's import path, for example through
pytest's `pythonpath` setting. Importing the script does not run its `main`.

`macrocosim.load_topology(path)` reads a topology document, such as a
`--topology` file, back into `MicrogridSpec` objects. A microgrid built or
edited in the UI is saved with "Save as .py", which writes a module with its
`make_*` calls and every id as `MICROGRID`, and a `main` that declares it when
the file is run: `python microgrid_7.py` brings the microgrid back on a
simulator, and `from microgrid_7 import MICROGRID` gives the spec to a script or
a test.

## Porting from Lisp

macrocosim 0.2 reads no Lisp. A `.lisp` argument, or a state directory with
`enterprise.lisp` or `microgrids/*.lisp`, stops the binary with a pointer to
this page. There is no converter; port a site file by hand:

*   **The generated block** (`make-microgrid` and its `%make-*` forms) becomes a
    `make_microgrid(...)` spec that `main` declares. Each keyword becomes an
    argument: replace `-` with `_`, drop the unit suffix, and wrap the value in
    its quantity type (`:capacity-wh 92000.0` is
    `capacity=Energy.from_kilowatt_hours(92)`). `:ramp-rate-w-per-s` and
    `:reactive-ramp-rate-var-per-s` keep their suffix and take floats. Symbols
    become enum members (`'error` is `mc.Health.ERROR`). `connect` forms become
    `successors=[...]`, and a component with two parents is one spec object in
    both lists. Keep the ids. Defaults fill what you leave out, as before.
*   **Lambda sources and `every` timers** become loops in `main`.
*   **`define-scenario`** becomes an async function, as above.

Two things behave differently:

*   **UI edits no longer persist on their own.** An edit of a script's microgrid
    in the UI lasts until the script runs again: its declaration rebuilds the
    microgrid from the script's spec, and Undo in the UI goes back to the edit.
    Every microgrid made in the UI or created by a script's declaration lives in
    memory only. "Save as .py" keeps either.
*   **A scenario must restore the knobs it changes.** A Lisp scenario put
    every knob it drove back when it stopped. A Python scenario is a plain
    function and puts them back itself, in a `finally`.

### Lisp functions and their replacements

| Lisp | Python |
|---|---|
| `make-microgrid`, `%make-*`, `connect`, `disconnect`, `remove-component`, `rename-component`, `set-microgrid-name`, `set-microgrid-tso` | `make_microgrid` (with `name`, `tso`, `grpc_port`), declared with `Connection.declare`; over HTTP, `POST /api/microgrids`, `PUT /api/mg/{mg}/spec`, `DELETE /api/mg/{mg}` |
| `set-meter-power`, `clear-meter-power` | `Meter.power.set(...)`, `.clear()` |
| `set-meter-reactive-power`, `set-meter-power-factor`, `clear-meter-reactive-power`, `clear-meter-power-factor` | `Meter.reactive_power.set(...)`, `.clear()`; a power factor in the spec (`power_factor`, `leading`) |
| `set-solar-sunlight`, `clear-solar-sunlight` | `SolarInverter.sunlight.set(...)`, `.clear()` |
| `set-battery-soc` | `Battery.soc.set(...)` |
| `set-boiler-demand-kg-per-s`, `set-boiler-demand`, `set-boiler-pressure` | `SteamBoiler.demand.set(MassFlow...)`, `SteamBoiler.pressure.set(Pressure...)` |
| `set-reactive-apparent-va`, `set-reactive-pf-limit` | `reactive_apparent_va` and `reactive_pf_limit` in `POST .../component/{id}/drive`; in the spec, `reactive_apparent` and `reactive_pf_limit` |
| `set-component-health`, `set-component-command-mode`, `set-component-telemetry-mode` | `Component.health`, `.command_mode`, `.telemetry_mode` |
| `set-component-operational-mode` | `Component.operational_mode` |
| `set-sample-lag-s`, `set-sample-lag-ms` | `Microgrid.set_sample_lag(timedelta)` |
| `cancel-all-streams` | `Microgrid.cancel_streams()` |
| `plug-ev`, `unplug-ev` | `EvCharger.plug(preset, ...)`, `EvCharger.unplug()` |
| `ev-info`, `ev-presets` | `GET .../component/{id}/ev` |
| `set-voltage-per-phase` | `Microgrid.set_voltage_per_phase(l1, l2, l3)` |
| `set-physics-tick-s`, `set-physics-tick-ms` | `make_microgrid(physics_tick=timedelta(...))` |
| `set-frequency`, `override-frequency-model`, `clear-frequency-override` | `Microgrid.frequency.set(...)`, which holds the value with no noise, and `.clear()`; to lean the frequency with its noise on, move the model's `nominal` |
| `set-frequency-model` | `Microgrid.set_frequency_model(nominal=..., mean_rev_rate=..., sigma=...)` |
| `current-frequency` | `GET /api/frequency` |
| `make-weather`, `set-weather`, `pass-cloud`, `weather-status` | `Microgrid.set_weather(...)`, `Microgrid.pass_cloud(...)`, `GET /api/mg/{mg}/weather`; `Microgrid.clear_weather()` removes it |
| `scenario-start`, `scenario-stop`, `scenario-record-csv`, `scenario-stop-csv` | `async with microgrid.record(name, csv=...)` |
| `scenario-event` | `Recording.event(kind, payload)` |
| `define-scenario`, `at`, `drive-*`, `ems`, `random-outage`, `scenario-elapsed`, `scenario-running-p`, `scenario-end-after-s` | an async function with `asyncio.sleep`, loops and `random` |
| `scenario-expect`, `check` | `signal.expect(matcher, timeout=..., hold_for=...)` |
| `set-active-power`, `set-reactive-power` | dropped: setpoints come from the app under test over gRPC |
| `augment-active-bounds`, `augment-reactive-bounds` | dropped from scripts; available over gRPC |
| `component-active-power`, `component-reactive-power`, `component-bound-lower`, `component-bound-upper`, `component-reactive-bound-lower`, `component-reactive-bound-upper` | `read()` and `expect` on the component's signals |
| `every`, `run-with-timer`, `cancel-timers` | asyncio tasks and `asyncio.sleep` |
| `now-seconds`, `parse-time-of-day`, `parse-offset`, `resolve-time`, `window-elapsed`, `random`, `set-random-seed`, `clear-random-seed`, `sin`, `cos`, `floor`, `ceiling`, CSV lookup | the standard library: `time`, `datetime`, `random`, `math`, `csv` |
| `set-enterprise-id`, `set-assets-socket-addr`, `set-dispatch-socket-addr`, `set-default-request-lifetime-s`, `set-default-augment-lifetime-s` | the binary's flags `--enterprise-id`, `--assets-addr`, `--dispatch-addr`, `--request-lifetime`, `--augment-lifetime` |
| `set-timezone` | dropped: the UI shows local time or UTC |
| `reset-microgrid` | no direct replacement. `Connection.unload(id)` (or `DELETE /api/mg/{mg}`), then declare it again, which creates it fresh. A declaration or a `PUT` of the same spec keeps every component's state |
| `load`, `watch-file`, `file-exists-p`, `current-source-file`, `current-microgrid-id`, `microgrid-name` | dropped: a script imports modules; a watcher such as `watchexec -r` re-runs it on a save |
