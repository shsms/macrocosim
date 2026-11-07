# Python scripts

A macrocosim script is a Python file that declares microgrids and animates
them. `macrocosim plant.py` starts the simulator, runs the script in a child
Python process, and runs it again each time you save it. The script uses the
same `macrocosim` client as a pytest test, so code moves freely between the
two.

This page describes the script shape, what a reload keeps, recording, and how
to port a Lisp site file. The client itself is described in the
[Python README](../python/README.md). Two complete scripts are in
[`examples/`](../examples/): [`starter_site.py`](../examples/starter_site.py)
and [`scenario_driving.py`](../examples/scenario_driving.py).

## Requirements

Python 3.11 or later, with the `macrocosim` package at exactly the binary's
version. The `macrocosim` wheel ships the binary, so a venv with the wheel has
both:

```sh
pip install macrocosim
macrocosim plant.py
```

The simulator looks for Python only when it is given a script, or the first
time a microgrid is exported. The first match wins:

1.  `--python PATH`;
2.  `$MACROCOSIM_PYTHON`;
3.  a `python` or `python3` next to the binary, which is the wheel's layout;
4.  `python3`, then `python`, on `PATH`.

It checks that one interpreter and does not try the next one, so a
misconfigured setup is reported instead of hidden. When the check fails, a
script run exits, and an export answers 503, with:

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
MICROGRIDS = [
    mc.make_microgrid(
        id=7,
        topology=mc.make_grid_connection_point(
            id=1, successors=[load, inverter]
        ),
    ),
]


async def run(microgrids: dict[int, mc.Microgrid]) -> None:
    plant = microgrids[7]
    async with plant.record("first hour", csv=True):
        for minute in range(60):
            kw = 20 + 10 * math.sin(minute / 60 * 2 * math.pi)
            await plant[load].power.set(Power.from_kilowatts(kw))
            await asyncio.sleep(60)
    await plant.grid_power.expect(
        mc.at_most(Power.from_kilowatts(40)), timeout=timedelta(seconds=30)
    )
```

A script defines two names:

*   **`MICROGRIDS`**, a list of `make_microgrid(...)` specs. Each needs an
    explicit `id`, so that each save replaces that microgrid instead of adding
    another. The specs are the same `make_*` specs a test passes to `launch`.
*   **`run`**, an async function that takes a `dict[int, Microgrid]` keyed by
    microgrid id. It gets the same `Microgrid` objects a test gets from
    `launch`, so `plant[load]` finds a component by its spec object, and
    every knob, signal and `expect` works as in a test.

The child runs the script as `__main__`, as `python plant.py` would, so an
`if __name__ == "__main__":` block in it runs too, before the simulator gets
the microgrids. Keep such a block out of a script. One that starts a
simulator of its own with `launch(...)` would start a second simulator under
`macrocosim plant.py`, and an `asyncio.run(...)` there fails the load,
because the child runs the script inside its own event loop.

The script's own directory comes first on the import path, so it can import
helper modules beside it; `scenario_driving.py` imports `starter_site.py`
this way. One script runs per simulator. A script with many microgrids can
split them across modules it imports.

Ctrl-C in the terminal stops the simulator. It first stops the child as a
reload does (see [Reloading](#reloading), step 4), also while the script is
still loading.

The child's output goes to the simulator's log, each line led by the
script's name (`plant.py: battery 1 tripped`; stderr at warn level), and so
to the UI's log panel. A line is read as UTF-8 whatever the locale, and one
longer than 4096 bytes is cut there. `GET /api/runner` gives the script's
state:

| Child state | Means |
|---|---|
| `starting` | started, its microgrids not sent yet |
| `running` | `run` is running |
| `finished` | `run` returned; the world stays as `run` left it |
| `crashed` | `run` raised; the traceback is in the output |

The result of the last reload is separate: `ok`, `failed_to_load` or
`pending`. A failed reload leaves the previous child running.

The binary's other flags still apply: `--state-dir`, `--ui-port`, and the
enterprise settings `--enterprise-id`, `--assets-addr`, `--dispatch-addr`,
`--request-lifetime` and `--augment-lifetime`. A script cannot be combined
with `--topology`, which boots microgrids from a topology document with no
script.

## Reloading

The simulator watches the `.py` files in the script's directory tree, so an
edit to a helper module reloads too. It skips hidden directories,
`__pycache__`, `site-packages`, `.venv`, `venv`, `target` and `node_modules`.
On a save, after 300 ms with no further `.py` change (a file of another kind,
such as a log the script writes all the time, does not hold a reload back):

1.  A new child starts, and the old one keeps running.
2.  The new child runs the script and sends its microgrids. If it fails to
    import, its `MICROGRIDS` or `run` is missing or wrong, or it sends
    nothing within 30 s, the reload is `failed_to_load`, the error and its
    file and line are in the output, and the old child and the world are
    untouched.
3.  The simulator checks every microgrid and builds it aside. An invalid
    spec, or an id another microgrid holds, fails the reload the same way.
4.  The old child is stopped: its stdin closes, and after 3 s it is killed.
    On Linux and macOS, the processes the script started (the app under
    test, say) go with it: the child leads a process group of its own,
    and once it has ended the group gets SIGTERM, then SIGKILL 1 s later.
    A process that moves to a process group or session of its own
    (`setsid`, `setpgid`, `Popen(process_group=0)` or
    `start_new_session=True`) leaves the group and keeps running, and a
    process in the group cannot read from the terminal. On Windows they
    keep running.
5.  The new microgrids are swapped in, and the new child starts `run`.

A child that crashed or failed to load is not started again until a watched
file changes, so a broken script does not loop.

A microgrid the script no longer lists is unloaded: its gRPC server stops,
its port is free again and its dispatches are deleted.

### What a reload keeps

Every reload rebuilds each of the script's microgrids in place. Its gRPC
server, physics and UI keep running, and each component keeps its state when
the new script has a component with the same id and an equal spec.

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

A UI edit of the microgrid's structure and an undo rebuild it the same way.

**Carry follows ids.** A component without an `id` gets an automatic one,
numbered from 1000 across the whole `MICROGRIDS` list. Adding or removing a
component renumbers every automatic id after it, in its microgrid and in the
later ones, and those components start fresh. Give an explicit id to every
component whose state should survive an edit of the topology: batteries, EV
chargers and steam boilers above all. The examples give every component one.

**Weather, grid voltage and sample lag** have no spec field, so a reload
keeps them. A script that stops calling `set_weather` keeps the old weather
until it calls `clear_weather()` or the simulator restarts; the same holds
for `set_voltage_per_phase` and `set_sample_lag`.

Component ids are unique across the simulator. A UI-made component takes an
id of 5000 or more, so it does not meet a script's automatic ids. A script
that claims an id another microgrid holds fails to load with both microgrids
named; give the component another explicit id.

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
animation also writes pauses that animation itself; `starter_site.py` does it
with a small `holding(...)` context manager the animation loops check. A
scenario puts back every knob it changed, in a `finally`:

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

Scenarios belong in the script: run one from `run`, awaited beside the
animation, and save. Only there can it pause the animation, which runs in the
script's process.

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

**When a rebuild ends it.** A UI edit, an undo or a script reload rebuilds the
microgrid and ends its recording, keeping the report up to the rebuild. The
block goes on, unrecorded:

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

`macrocosim.connect(url)` attaches to a running simulator and yields its
microgrids by id. In `python -m asyncio`, where `await` works at the top
level:

```python
from frequenz.quantities import Power

import macrocosim as mc

async with mc.connect("http://127.0.0.1:8801") as microgrids:
    plant = microgrids[7]
    await plant[2].power.set(Power.from_kilowatts(20))
```

The microgrids are built from the simulator's copy of their specs, so look
components up by id (`plant[2]`): the spec objects of a script module you
import are other objects. Leaving the block closes the connections and leaves
the simulator running.

A terminal is for one-off reads and writes. A write stands until the next
write to the same knob, so a write to a knob the script animates is
overwritten by the animation a moment later. Scenarios belong in the script
(see [Animation and scenarios](#animation-and-scenarios)).

## Specs in tests

A test imports a script's specs and launches them with `launch`, which starts
a private simulator for one microgrid on free ports:

```python
from plant import MICROGRIDS


async def test_the_plant_holds_its_peak() -> None:
    async with mc.launch(MICROGRIDS[0]) as plant:
        ...
```

The script's directory must be on the test's import path, for example through
pytest's `pythonpath` setting. `run` does not run under pytest: the test
drives the microgrid itself.

`macrocosim.load_topology(path)` reads a topology document, such as a
`--topology` file, back into `MicrogridSpec` objects. A microgrid built or
edited in the UI is saved with "Save as .py", which writes a module with its
`make_*` calls, every id and a `MICROGRIDS` list that a script or a test
imports.

## Porting from Lisp

macrocosim 0.2 reads no Lisp. A `.lisp` argument, or a state directory with
`enterprise.lisp` or `microgrids/*.lisp`, stops the binary with a pointer to
this page. There is no converter; port a site file by hand:

*   **The generated block** (`make-microgrid` and its `%make-*` forms) becomes
    `MICROGRIDS`. Each keyword becomes an argument: replace `-` with `_`, drop
    the unit suffix, and wrap the value in its quantity type
    (`:capacity-wh 92000.0` is `capacity=Energy.from_kilowatt_hours(92)`).
    `:ramp-rate-w-per-s` and `:reactive-ramp-rate-var-per-s` keep their
    suffix and take floats. Symbols become enum members (`'error` is
    `mc.Health.ERROR`). `connect` forms become `successors=[...]`, and a
    component with two parents is one spec object in both lists. Keep the
    ids. Defaults fill what you leave out, as before.
*   **Lambda sources and `every` timers** become loops in `run`.
*   **`define-scenario`** becomes an async function, as above.

Two things behave differently:

*   **UI edits no longer persist on their own.** An edit of a script's
    microgrid in the UI lasts until the script's next save replaces it, and
    a microgrid made in the UI lives in memory only. "Save as .py" keeps
    either.
*   **A scenario must restore the knobs it changes.** A Lisp scenario put
    every knob it drove back when it stopped. A Python scenario is a plain
    function and puts them back itself, in a `finally`.

### Lisp functions and their replacements

| Lisp | Python |
|---|---|
| `make-microgrid`, `%make-*`, `connect`, `disconnect`, `remove-component`, `rename-component`, `set-microgrid-name`, `set-microgrid-tso` | `MICROGRIDS` (`make_microgrid` with `name`, `tso`, `grpc_port`), edited and saved; over HTTP, `POST /api/microgrids`, `PUT /api/mg/{mg}/spec`, `DELETE /api/mg/{mg}` |
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
| `reset-microgrid` | no direct replacement. A script's microgrid: `DELETE /api/mg/{mg}`, then save the script, which creates it fresh. One made in the UI: `DELETE`, then `POST /api/microgrids`. A `PUT` of the same spec keeps every component's state |
| `load`, `watch-file`, `file-exists-p`, `current-source-file`, `current-microgrid-id`, `microgrid-name` | dropped: a script imports modules, and its own directory is watched |
