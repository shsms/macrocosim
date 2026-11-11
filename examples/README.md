# Examples

Short Python scripts, each showing one feature of macrocosim, and pytest files
that test an app. Each script declares its own small microgrid with its own ids,
so you can run several against one simulator at once and watch them side by side
in the UI. The grid frequency is one value for the whole simulator, though, so
`sites/grid_voltage.py` and `sites/two_microgrids.py` change it for every
microgrid.

"Your app" in these examples is the control app you test with macrocosim: it
reads the microgrids and commands their components over gRPC, as it would at a
real site. The scripts only change the world around it.

Create a virtual environment with [uv](https://docs.astral.sh/uv/) and install
the `macrocosim` package, 0.2 or later. Its wheel brings the simulator too, as
the `macrocosim` command, with `macroctl`, a command-line gRPC client:

```sh
uv venv
source .venv/bin/activate
uv pip install "macrocosim>=0.2"
```

In a checkout of this repository, install the package from source instead. That
builds the simulator, so it needs the Rust toolchain, `protoc` and the proto
submodules (`git submodule update --init --recursive`). `--reinstall` rebuilds
it after you pull changes:

```sh
uv pip install --reinstall ./python
```

Start the simulator, then run any script in another terminal with the same
environment activated, for example:

```sh
macrocosim
python examples/assets/ev_charging.py
```

Each script connects to `$MACROCOSIM_URL`, else `http://127.0.0.1:8801`,
declares its microgrid, plays a short sequence and exits. The microgrid stays
loaded until you unload it (`DELETE /api/mg/{id}`) or stop the simulator. The
[scripting guide](../docs/python-scripts.md) explains the API they use.

## Assets

| Script | Microgrid | Shows |
|---|---|---|
| [`assets/battery_soc_window.py`](assets/battery_soc_window.py) | 11 | the SoC window and how it narrows the battery's bounds |
| [`assets/ev_charging.py`](assets/ev_charging.py) | 12 | plugging a catalog car in, taper and target SoC |
| [`assets/solar_weather.py`](assets/solar_weather.py) | 13 | PV under the site's weather, a passing cloud, sunlight set by hand |
| [`assets/chp_and_boiler.py`](assets/chp_and_boiler.py) | 14 | a CHP's meter, steam demand and boiler pressure |

## Testing an app

The tests in `testing/` are pytest files. Each launches its own simulator with
`mc.launch`, so they need no running simulator. `testing/pytest.ini` turns on
pytest-asyncio's auto mode; copy it, or its `[tool.pytest.ini_options]`
equivalent, with the tests. Install pytest first:

```sh
uv pip install pytest pytest-asyncio
pytest examples/testing
```

| File | Shows |
|---|---|
| [`testing/test_first.py`](testing/test_first.py) | the shortest test: launch, set a load, `expect` the grid power |
| [`testing/conftest.py`](testing/conftest.py) | a fixture that launches a spec and stops it after the test |
| [`testing/test_demo.py`](testing/test_demo.py) | tests that use that fixture |
| [`testing/test_recording.py`](testing/test_recording.py) | recording a run with events and CSV files, and checking its report |
| [`testing/test_peak_shaving.py`](testing/test_peak_shaving.py) | starting your app (`APP_CMD`) beside the simulator, and checking what it achieves |

## Faults

| Script | Microgrid | Shows |
|---|---|---|
| [`faults/health_trips.py`](faults/health_trips.py) | 31 | inverters tripping on `error` and `standby`, and recovering |
| [`faults/telemetry_modes.py`](faults/telemetry_modes.py) | 32 | silent, error-empty, closed and not-found telemetry |
| [`faults/command_modes.py`](faults/command_modes.py) | 33 | commands that time out, fail or are refused, with a gateway delay and ramp |
| [`faults/lag_and_streams.py`](faults/lag_and_streams.py) | 34 | stale sample timestamps and cancelled telemetry streams |

## Sites and the grid

| Script | Microgrid | Shows |
|---|---|---|
| [`sites/two_microgrids.py`](sites/two_microgrids.py) | 41, 42 | two microgrids in one `declare`, and the frequency they share |
| [`sites/grid_voltage.py`](sites/grid_voltage.py) | 43 | phase voltages, an unbalanced phase, a held frequency |
| [`sites/formula_shapes.py`](sites/formula_shapes.py) | 44, 45, 46 | topologies that formula code tends to get wrong |

The scripts that print readings or wait with `expect` read the simulator's own
samples. Your app reads the same microgrids over gRPC at the address the UI's
microgrid card shows.
