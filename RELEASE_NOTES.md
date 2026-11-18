# macrocosim 0.2.0 Release Notes

## Summary

macrocosim no longer embeds Lisp. A Python script is a plain program that connects to a running simulator, declares its microgrids and animates them: run `macrocosim`, then `python plant.py`, as often as you like. Each run updates the microgrids it declares, leaves an unchanged one as it is, and keeps the state of each component whose spec did not change. The engine builds microgrids from a JSON topology document that declarations, `--topology`, the HTTP API and the Python client share.

The Python client is rewritten around one async API. Every number carries its unit in its name, and the same quantity has the same name in the topology document, HTTP, the WebSocket, macroctl and Python. The HTTP API has one route tree and one error format. Lisp files from 0.1 are refused, and clients need the changes listed under Upgrading.

## Upgrading

### Lisp is gone

- macrocosim 0.2 reads no Lisp; support ended with 0.1. Before anything binds, the binary stops with a message naming the files when it is given a `.lisp` argument, or when its state directory holds `enterprise.lisp` or `microgrids/*.lisp`.
- The state directory is the one `--state-dir` names, or the current directory without it. So a run started from a directory that keeps 0.1 site files is refused until you pass another `--state-dir` or move the files.
- Port each site file to a Python script by hand; there is no converter. [`docs/python-scripts.md`](docs/python-scripts.md) is the porting guide: it maps every Lisp function to its replacement, and says what behaves differently. To stay on Lisp, pin `macrocosim<0.2` or keep the 0.1 binary.
- Removed with the interpreter: the REPL panel and its eval routes (`/api/eval`, `/api/mg/{mg}/eval`, `/api/format`, `/api/symbols`); the Load script dialog and `/api/load`, `/api/load-as`, `/api/scripts` and `/api/mg/{mg}/adopt`; snapshots (the Snapshots dialog and `/api/mg/{mg}/snapshots`); scenarios (`define-scenario`, the Scenarios mode, `/api/scenarios`, `/api/scenarios/stop`, `/api/scenarios/{name}/start` and `/api/mg/{mg}/scenario...`); the managed microgrid files and their overrides; `/api/clock`; the `config_error` WebSocket event; and the `macrocosim-etags` binary with its `etags` Cargo feature.
- The deterministic, serverless gate (`macroctl scenario run --stepped`) is gone, with the stepped clock. Scripts and tests run in real time; [`docs/e2e-testing.md`](docs/e2e-testing.md) has the CI recipes.
- A scenario is now an async function in a script, run from its `main`. It records itself with `microgrid.record(...)` (see Recording, below) and checks with `expect`. Unlike a Lisp scenario, it does not put back the knobs it drove when it ends: restore them in a `finally`.
- UI edits are no longer written to a file. An edit of a script's microgrid lasts until the script runs again: its declaration rebuilds the microgrid from the script's spec, and Undo in the UI goes back to the edit. Every microgrid made in the UI or created by a script's declaration lives in memory only, until it is unloaded or the simulator stops. Export either with *Copy as Python* or *Save as .py*.
- Nothing reloads when a file is saved: 0.1 reloaded the world on every save of a site file. Run the script again, or let a watcher re-run it on every save, such as `watchexec -r -e py -- python plant.py`.
- The server has no time zone (`set-timezone` is gone). The zone chip switches the UI between the browser's zone and UTC. gRPC and the HTTP API use UTC, as before.
- The `--state-dir` directory now holds only the CSV files of recordings, under `recordings/`. The enterprise settings that `enterprise.lisp` held are command-line flags: `--enterprise-id`, `--assets-addr`, `--dispatch-addr`, `--request-lifetime` and `--augment-lifetime`.

### Rust library

- The `macrocosim` Rust library is not a supported API: it serves the binaries and their tests, and the crate is not published to crates.io. Its `lisp`, `sim::axis`, `sim::clock` and `sim::scenarios` modules are gone.
- The `proto` re-exports follow the vendored protos: `proto::assets` is now `platformassets::v1alpha1`, and `ElectricalComponentOperationalMode::TelemtryOnly` is `TelemetryOnly`.

### HTTP routes

- Everything about one microgrid is under `/api/mg/{mg}/`. The routes without a microgrid (`/api/topology`, `/api/component`, `/api/weather`, `/api/history`, `/api/setpoints`, `/api/microgrid/*`) are gone.
- `/api/mg/{mg}/microgrid/{status,latest,history,formulas}` is now `/api/mg/{mg}/metrics/{status,latest,history,formulas}`.
- A component's state, history and setpoints are at `/api/mg/{mg}/component/{id}`, `.../history` and `.../setpoints`.
- `POST /api/microgrids/create` is now `POST /api/microgrids`. It takes a topology document, `{"schema_version": 1, "microgrids": [...]}`. 0.1's `name`, `id`, `grpc_port` and `tso` body answers 422, as does a document with no microgrids. The answer is 201 with `{"microgrids": [{"id", "grpc_port", "runtime"}, ...]}`, one entry per microgrid created. The microgrids live in memory.
- `GET /api/defaults` answers one object keyed by component type (`meter`, `battery`, ...). Each holds the fields the engine fills into a spec, under their topology-document names. 0.1's `{"entries": [{"category", "var_name", "value"}]}` list of Lisp variables is gone.
- Every failure is JSON, `{"error": "..."}`. An unknown path answers 404 and a wrong method answers 405, both with that body. A success that has nothing to return answers 204, and no body carries `ok`.

### HTTP fields

Old field names are not accepted. A request body with an unknown field answers 422 with a JSON error that names the field. A query string with an unknown field answers 400.

- Component state: `setpoints[].remaining_ms` is `remaining_s`, in seconds. `envelope.active` and `envelope.reactive` are `envelope.active_w` and `envelope.reactive_var`.
- Component state: `knobs[]` entries no longer carry `expr`, the printed Lisp of a dynamic value.
- `GET /api/microgrids` entries no longer carry `managed`; `source` and `unsaved` remain.
- Drive (`POST .../component/{id}/drive`): `reactive_var` is `reactive_power_var`. `steam_demand_kg_h` is `steam_demand_kg_per_s`, in kg/s.
- Dispatches: `start_ms` is `start` in the request and the response. `end_ms` is `end` in the response; the request has no `end`. `create_ms` and `update_ms` are `created_at` and `updated_at` in the response. All of them are RFC 3339 strings.
- History and setpoints answer with `component_id`, not `id`. A history `samples` pair is `[t_s, value]`, with `t_s` in epoch seconds.
- History (`GET .../component/{id}/history`) answers 404 for an unknown component. It answered 200 with no samples, the same as a component that has not been sampled yet.
- `metrics/latest`: `ts_ms` of each stream is `ts`, an RFC 3339 string.
- `metrics/history`: each entry is `{t_s, value}`, with `t_s` in epoch seconds.
- Logs: `ts_ms` is `ts`, an RFC 3339 string.
- Weather, request and response: `cloud_depth`, `cloud_duration` and `cloud_ramp` are `cloud_depth_pct`, `cloud_duration_s` and `cloud_ramp_s`. `cloud_rate_per_h` is `cloud_mean_gap_s`, the mean seconds between clouds. In a request, `cloud_mean_gap_s: 0` turns ambient clouds off. The response has `null` when there are none. `pct` is `sunlight_pct`.
- Status (`POST .../component/{id}/status`): `health` no longer takes `ready`. Use `ok`.
- Mode values are spelt in snake case, as in the topology document: `over_bound`, `error_empty`, `not_found`, `telemetry_only`, `control_only` and `control_and_telemetry`. This applies to `status` requests and the `topology` payload. The hyphenated forms (`over-bound`) are refused with 400.
- Import (`POST /api/microgrids/import`): `mid` is `id`.
- Formula query: `ids` is `component_ids`.
- A generic `value` now has a `unit` beside it: knobs and setpoints. The reactive unit is `"VAr"`.
- The `unit` of a reactive stream in `history` and `metrics/latest` is `"VAr"`. It was `"var"`.
- The `boiler-demand` knob `value` in a component's `knobs[]` is in kg/s. It was kg/h, so divide an old value by 3600.

### WebSocket

- `mg_id` is `microgrid_id` on every event that belongs to a microgrid.
- `id` is `component_id` on `sample`, `setpoint` and `knob_changed`.
- `ts_ms` is `ts`, an RFC 3339 string, on every event that carries a time.
- `sample`, `setpoint` and `knob_changed` carry a `unit`.
- The `unit` of a reactive `microgrid_sample` is `"VAr"`. It was `"var"`.
- The `boiler-demand` `knob_changed` `value` is in kg/s. It was kg/h.
- `knob_changed` no longer carries `expr`, the printed Lisp of a dynamic value. It also reports the `operational-mode`, `grid-voltage` and `sample-lag` knobs. Its `component_id` is null for a knob of the whole microgrid (`grid-voltage`, `sample-lag`), and its `value` is a number, a string (`operational-mode`) or a list of three phase voltages (`grid-voltage`).

### Endpoints file

`--emit-endpoints` writes `microgrids[].grpc_addr`. It was `microgrids[].grpc`. It also writes `version`, the binary's version in PEP 440 form, for example `0.2.0`. It lists the microgrids the simulator booted with (`--topology`); microgrids declared later are in `GET /api/microgrids`, with their runtime's `grpc_addr`.

### Example world

The Lisp examples are gone. [`examples/`](examples/README.md) holds short Python scripts, one feature each: assets (battery, EV charging, PV and weather, CHP and steam boiler), testing an app with pytest and recordings, fault injection, and sites and the grid. Each script declares its own microgrid, so several can run against one simulator. `python/examples/` is gone too: its pytest demo is now in [`examples/testing/`](examples/testing/). The 0.1 Lisp demo world (`berlin-demo.lisp`, `scenario-driving.lisp`) has no direct replacement.

### macroctl

- The `scenario` and `snapshot` commands are removed, with every command that evaluated Lisp. The gRPC commands, `info`, `dispatch`, `pool` and `dashboard` stay.
- `set-power --lifetime` is `--lifetime-s`, in whole seconds.
- `set-power --reactive` is removed. Use the new `set-reactive-power`.
- `augment-bounds --reactive` is removed. Use the new `augment-reactive-bounds`, with `--lower-var` and `--upper-var`.
- `augment-bounds --lower`, `--upper` and `--lifetime` are `--lower-w`, `--upper-w` and `--lifetime-s`.
- `dispatch create --duration` is `--duration-s`.
- `dashboard --interval` is `--interval-s`.
- `list --id` is `--component-id`.
- `dispatch list`, `get`, `create`, `pause`, `resume` and `delete` no longer take the microgrid id as an argument. Use the global `--microgrid-id N`, for example `macroctl --microgrid-id 2 dispatch list`. Without it, the lowest microgrid id is used.
- `--addr` no longer defaults to `http://[::1]:8800`. Without it, the gRPC commands ask the UI server (`--ui-addr`) for the address of the microgrid's gRPC server. Pass `--addr` to name a server directly.
- Printed tables still scale to kW and kWh. They print the unit of electrical metrics, SoC and humidity; ratios (power factor, THD), temperature, wind, pressure, irradiance and an unspecified metric print none. `list` labels its columns `rated_lower_w` and `rated_upper_w`, and `stream` has a unit column.

### Assets API

- The Assets API server on `[::1]:9900` serves `frequenz.api.platformassets.v1alpha1.PlatformAssetsService` (frequenz-api-assets v0.4.0), as the production Assets API does. It was `frequenz.api.assets.v1.PlatformAssets` (v0.1.0), which clients can no longer reach. Move your app to a client built on frequenz-api-assets v0.4.0 or later, and put the `List*` request filters in their `filter` message.
- `MACROCOSIM_PROTO_ROOT` now overrides only the microgrid proto and its google protos: frequenz-api-common always comes from the assets submodule (v0.8.11). A private mirror that brought its own frequenz-api-common needs one compatible with v0.8.11.
- The server also answers `ListMicrogrids`. `ListGridpools` and `ListMarketTopologyRelations` answer with empty lists, `ListGridpoolEnergySchedules` with NOT_FOUND, and `ListMicrogridSensors` with an empty list for a known microgrid: the simulator has no gridpools, market relations or sensors.

### Python

The Python client is rewritten. 0.1 code needs changes; the table under Old and new names maps the old names to the new ones.

#### Removed

- The sync client (`Site`, `mc.launch` as a plain context manager) and the `macrocosim.aio` namespace: there is one async API at the top level.
- The lowercase builders (`grid`, `meter`, `battery`, ...) that became live objects at launch, and `Microgrid(...)` as a builder.
- The Python scenario API (`Scenario`, `Check`, `ScenarioRun`, `ScenarioReport`, `JournalEvent`, `run_scenario_stepped`) and the pytest plugin with its `macrocosim_scenario` marker. A recording (`Microgrid.record`) and `expect` take their place.
- `plug_ev`, `unplug_ev`, `ev_info` and `EvPreset`. A live `EvCharger` has `plug(preset, ...)` and `unplug()`.
- Lisp in the API: `raw(...)`, `RawLisp`, `**extra`, `to_lisp()`, `to_lisp_atom`, `LispRenderable`, `ConfigSource`, `LaunchConfig`, `from_lisp_file`, `eval`, and launching a `.lisp` file.
- Commands from tests: `set_active_power`, `augment_*`, `command()`, `SetpointRejected`.
- `CumulativeSignal`, `stored_energy`, `drive(...)`, `status(...)`, the `microgrid_id` parameters, and the gRPC dependency.
- 0.1's `connect` and `MicrogridEndpoint`. The new `connect()` attaches to a running simulator and yields a `Connection` (see New Features).
- Launching several microgrids at once. Each `launch` now starts one microgrid.
- `ComponentHandle`, `Metric`, `MetricSpec` and `Schedule`.
- `Signal.try_read`, `Site.read_until`, the `poll=` argument of `expect`, and `launch(bin=...)`. To use another binary, set `MACROCOSIM_BIN`.
- Every 0.1 submodule: `macrocosim.runtime`, `.build`, `.handles`, `.scenarios`, `.signals`, `.metrics`, `.matchers`, `.enums`, `.errors`, `.assertions`, `.pytest_plugin` and `.aio`. Import from `macrocosim` only.

#### Changed

- Specs are inert and frozen, built by `make_*` constructors, and compared by identity; `launch` yields a `Microgrid`, and `microgrid[spec]` or `microgrid[id]` gives the live component. `id=` is optional; automatic ids may change between versions.
- `launch` starts the simulator from the spec's topology document (`--topology`), and raises `LaunchError` naming both versions when the binary's version is not the client's.
- All reads go over the simulator's HTTP API at 1 Hz: component reads from its own sampler, aggregates from its loopback. Component reads do not show telemetry faults; aggregates do.
- `expect(matcher, timeout=, hold_for=)` waits for the first matching sample, then requires every sample for `hold_for` to match, judged on the simulator's timestamps; it returns `None`. `read(timeout=)` replaces `read(wait=)`. It returns the first sample with a value that is newer than the newest sample the simulator had when the call started.
- `launch` takes `env=` (laid over the environment for the simulator only) and `state_dir=` (kept, with the log); without `state_dir` the temporary directory is always deleted.
- The binary bundled in the wheel is found before one on `PATH`.
- An aggregate whose category the topology lacks (no solar, say) reads 0 instead of having no data.
- Errors: the client's own errors derive from `MacrocosimError`. `SpecError` is raised for a spec that breaks one of the client's rules (for example an id, a pairing or a fuse current), `UnknownComponent` for a lookup of a component the microgrid does not have, `MicrogridClosed` for a microgrid used after its block (`launch` or `connect`) has exited, and `ConnectionClosed` for a `Connection` used after its `connect` block has exited. `LaunchError`, `ServerError` and `SimulatorExited` are raised for failures of the simulator or its HTTP API, `Rejected` when the simulator refuses a set or a fault, `NoSample` when a `read` or `expect` gets no usable sample, and `ExpectationFailed` when the samples disagree.
- A wrong argument type raises `TypeError`, and a wrong value `ValueError`. When a spec is built, a value that breaks one of the client's rules raises `SpecError`, a `ValueError` subclass. A NaN or infinite value, or an int too large for a float in a float field (`power_factor`, say), raises a plain `ValueError`.
- Exception bases changed. `Rejected` is not a `ValueError`; the old `ControlRejected` and `EvalRejected` were. `launch` raises `LaunchError`; it raised `RuntimeError`, `TimeoutError` or `FileNotFoundError`. An `expect` that gets no sample raises `NoSample`, which is not an `AssertionError`; an `ExpectationFailed` is.
- `DrivenSignal` and `SettingSignal` are replaced by `SettableSignal`, `ClearableSignal`, `Knob` and `ClearableKnob`.
- `mc.Component` now names the live component class, not the base class of the builders.
- A matcher is now any object with `matches()` and `describe()`. In 0.1 it was a dataclass with the fields `approx`, `tol`, `within`, `max` and `min`.
- An energy `expect` now polls until its timeout, like a power `expect`. 0.1's `CumulativeSignal` checked the value once.
- The values of `TelemetryMode`, `CommandMode` and `OperationalMode` members are snake case, as the simulator spells them: `TelemetryMode.ERROR_EMPTY` is `"error_empty"`, not `"error-empty"`. Code that compares a member with a string literal needs the new spelling.

#### Compatibility

- The `grpc` extra is gone; the app being tested depends on `frequenz-client-microgrid` itself. The package ships `py.typed`.
- The `grpc` extra also installed pytest, pluggy and pytest-asyncio. Install `pytest` and `pytest-asyncio` yourself, and set `asyncio_mode = "auto"` (see python/README.md).

#### Old and new names

| 0.1 | 0.2 |
|---|---|
| `mc.Microgrid(id=1, topology=mc.grid(id=1, successors=[...]))` | `mc.make_microgrid(id=1, topology=mc.make_grid_connection_point(id=1, successors=[...]))` |
| `mc.meter(...)`, `mc.battery(...)`, `mc.battery_inverter(...)`, `mc.solar_inverter(...)`, `mc.chp(...)` | `mc.make_meter(...)`, `mc.make_battery(...)`, `mc.make_battery_inverter(...)`, `mc.make_solar_inverter(...)`, `mc.make_chp(...)` |
| `mc.ev_charger(...)`, `mc.steam_boiler(...)` | `mc.make_ev_charger(...)`, `mc.make_steam_boiler(...)` |
| `rated=(lo, hi)` on `grid`, `battery`, `battery_inverter` or `solar_inverter` | `rated_lower=lo, rated_upper=hi` on the matching `make_*` |
| `async with mc.aio.launch(topology) as site:` | `async with mc.launch(spec, env=..., state_dir=...) as microgrid:` |
| `site.grpc_url`, `site.grid_power` | `microgrid.grpc_url`, `microgrid.grid_power` |
| `builder.power.set(x)` after launch | `microgrid[builder].power.set(x)` |
| `expect(approx=x, tol=t)`, `max=`, `min=`, `within=` | `expect(mc.near(x, tol=t))`, `mc.at_most`, `mc.at_least`, `mc.between` |
| `for_=` | `hold_for=` |
| `read(wait=...)` | `read(timeout=...)` |
| `plug_ev(...)`, `unplug_ev()` | `microgrid[charger].plug(preset, ...)`, `.unplug()` |
| `ControlRejected`, `EvalRejected` | `Rejected`; eval is gone |
| `DrivenSignal`, `SettingSignal` | `SettableSignal`, `ClearableSignal`, `Knob`, `ClearableKnob` |
| `mc.raw(...)`, `**extra`, scenarios, commands, the pytest plugin | removed |

## New Features

- Python scripts. A script is a plain program: `async with mc.connect() as sim:` attaches to a running simulator, and `await sim.declare(spec)` declares a microgrid on it and returns the same `Microgrid` a test gets from `launch`. `declare(a, b, ...)` declares several in one request and returns a tuple; `await sim.unload(id)` unloads one; `await sim.microgrids()` lists what is loaded, by id. Without a URL, `connect` uses `$MACROCOSIM_URL`, else `http://127.0.0.1:8801`.
- A declaration works per microgrid id, with no owner: a new id is created; the same spec as the loaded one changes nothing, so the app being tested keeps its streams and setpoints and every knob keeps its value; a changed spec rebuilds the microgrid with carry. Nothing is unloaded on its own, and declared microgrids stay loaded after the script ends. Each microgrid needs an explicit id. A watcher such as `watchexec -r -e py -- python plant.py` re-runs a script on every save.
- A declaration carries the package's version, and the simulator refuses another version with a message that names both. Over HTTP it is `POST /api/microgrids?existing=rebuild&client_version=V`, which answers each microgrid's `outcome`: `created`, `rebuilt` or `unchanged`.
- A declaration also starts the gRPC server of a microgrid whose start failed (its port was taken), even when the spec did not change.
- Carry. A rebuild, from a declaration, a UI edit or an undo, keeps a component's battery SoC, plugged car, boiler pressure, history and energy totals when the new spec has a component with the same id and an equal spec, defaults filled. Its name, health and modes don't count, so a rename keeps the state.
- Carry follows component ids. Automatic ids are numbered from 1000, continuing across one connection's `declare` calls, so they depend on the order of the calls and shift when a component is added. A shifted id can collide with a microgrid a later call declared on the previous run, and an earlier call can take an explicit id a later call gives: the declaration that finds the id held is then refused. Declare all of a script's microgrids in one `declare(a, b, c)` call, or give explicit ids. Give an explicit id to every component whose state should survive an edit of the topology, or that the app being tested addresses.
- On every rebuild, knobs start from the spec; weather, grid voltage and sample lag stay until changed or cleared. A rebuild ends the microgrid's gRPC telemetry streams, so a client has to open them again.
- Component ids are unique across the engine. A declaration whose component claims an id or a gRPC port held by a microgrid the declaration does not list is refused with a message that names that microgrid and how to unload it. This is what a microgrid declared under a new id runs into, while the old one is still loaded. UI-made components take the lowest free id of 5000 or more.
- The topology document. `--topology PATH` boots the microgrids of a JSON topology document. `GET /api/mg/{mg}/spec` returns a microgrid's spec, and `PUT` rebuilds the microgrid from a new one, with carry; a changed `grpc_port` moves its gRPC server.
- `GET /api/defaults` serves the defaults the engine fills into a spec, which the Defaults panel shows read-only, and `GET /api/spec/schema` the document's JSON Schema. `mc.load_topology(path)` reads a document back into specs.
- Unloading a microgrid. `DELETE /api/mg/{mg}` cancels its telemetry streams, stops its gRPC server (freeing the port), its physics and history tasks, its loopback and its recording, deletes its dispatches with a `Deleted` event each, frees its component ids and sends `microgrid_unloaded` on the WebSocket.
- Recording. `async with microgrid.record(name, csv=False) as recording:` records the block's metrics: peak grid power, battery charge and discharge, PV energy, SoC statistics and 15-minute averages of the grid power. `recording.event(kind, payload)` adds a named event, and `recording.report()` returns a `RecordingReport` after the block.
- With `csv=True`, every component's telemetry goes to CSV files under `recordings/<microgrid id>/<name>-<UTC time>/` in the state directory; a directory that exists already gets a `-2`, `-3`, ... suffix. The routes are `/api/mg/{mg}/recording`, `.../start`, `.../stop`, `.../event`, `.../events`, `.../report`, `.../csv` and `.../csv/{file}`, and the Report panel shows the recording while it runs.
- A rebuild ends a recording and keeps its report, with `ended_by: "rebuild"`; a script recording it logs a warning and goes on.
- Exporting a microgrid as Python. `GET /api/mg/{mg}/spec.py` renders the stored spec as a Python module with its `make_*` calls and every id as `MICROGRID`, which a script or a test imports, and a `main` that declares it when the file is run. The UI's microgrid bar has *Copy as Python* and *Save as .py*. The simulator runs Python only for this export, and finds it with `--python PATH`, `$MACROCOSIM_PYTHON`, a `python` beside the binary (the wheel's layout) or `PATH`, in that order; only the first match is checked.
- `mc.connect()` also works for one-off reads and writes from `python -m asyncio`.
- Typed routes for every knob a script drives: `POST .../component/{id}/runtime` (the operational mode), `reactive_apparent_va` and `reactive_pf_limit` on `POST .../component/{id}/drive`, `POST`/`DELETE .../component/{id}/ev` (plug and unplug a car), `POST .../grid` (phase voltages), `POST .../sample-lag`, `POST .../streams/cancel`, `DELETE .../weather`, a `seed` for the weather, and `GET`/`POST /api/frequency` for the frequency all microgrids share.
- The Python client drives each, except an inverter's reactive limits and a meter's power factor, which a script sets in the spec. EV chargers, steam boilers (with `Pressure` and `MassFlow`), wind turbines, power transformers and breakers have specs and live classes. A `Microgrid` has `set_weather`, `pass_cloud`, `clear_weather`, `set_voltage_per_phase`, `set_sample_lag`, `cancel_streams`, `frequency` and `set_frequency_model`.
- The UI edits a microgrid through its spec: every structural edit reads the spec, changes it and `PUT`s it, and undo and redo step through the stored specs. An *edited here* chip marks a `--topology` microgrid changed since it booted, and an *in memory only* chip a microgrid made in the UI or created by a script's declaration.
- One simulator supports 1600 knob writes per second in total (measured with `tools/write-rate.py`); see python/README.md.
- The grid connection point honours a rated band given on one side only. The given side applies and the other stays open.
- [`docs/names-and-units.md`](docs/names-and-units.md) lists the rules and the name of every quantity in the topology document, HTTP, macroctl and Python.
- macroctl has `set-reactive-power` and `augment-reactive-bounds`.
- `macroctl from-assets MICROGRID_ID` fetches a microgrid from an Assets API and writes a Python module that declares it, as *Save as .py* does, or with `--json` its topology document. It reads the API's URL, key and signing secret from `$ASSETS_API_URL`, `$ASSETS_API_AUTH_KEY` and `$ASSETS_API_SIGN_SECRET`; set them without typing the secret at the prompt, such as by sourcing a file only you can read. Then run `macroctl from-assets 42 -o site_42.py`. The flags `--url`, `--auth-key` and `--sign-secret` work too, but a flag shows to every user in `ps`.
- The module keeps the API's ids. Components and connections outside their operational lifetime, and components the simulator has no model for, are left out and named on stderr. Each parent of a component the simulator has no model for is connected to each of its children. A microgrid with more than one grid connection point is refused, and a component that is not under the grid connection point, over the connections that are left, is left out too.
- A hybrid inverter, and an inverter of no type over a battery (the battery is its child, or only components with no model are in between), are left out with the components only such inverters lead to, such as their batteries. The module writes them commented out: a hybrid inverter to uncomment once the simulator can model one, and an inverter of no type as a battery inverter, to uncomment once you know it is one. They go under the first of the inverter's parents (in the API's connection order) that is left and can have components under it: the grid connection point, a meter, a breaker, a power transformer or a battery inverter. An inverter with no such parent is not written, and stderr says so; `--json` leaves these inverters out.
- `GET /api/mg/{mg}/component/{id}/ev` reads the car plugged into a charger: `plugged`, `presets`, and when plugged `preset`, `soc_pct`, `target_soc_pct`, `phases`, `max_current_a`, `capacity_wh`, `energy_wh`, `plugged_at` and `state`. It answers 404 for an unknown component and 400 for one that is not a charger.
- Every time the UI shows uses the zone the zone chip in the top bar picks: the browser's zone, or UTC. This covers the charts, the log tail, the inspector's setpoints, the weather chart and cloud list, the recording report and the dispatches.
- The weather panel's sunrise and sunset fields stay in UTC, as the weather route takes them. Times in the HTTP and gRPC APIs stay UTC.
- Component counts in the UI include hidden components and say how many are hidden, for example `12 components (1 hidden)`. The header left them out while the microgrid card counted them. The header's connection count includes the hidden components' connections too. The loopback pill in the top bar shows `✓ connected` and no longer a count.
- Each `GET /api/microgrids` entry has `hidden_component_count`, the number of its components that are hidden.
- The UI has a light theme. By default it follows the OS setting. The theme chip in the top bar picks auto, light or dark, and the browser remembers the choice.
- The UI is set in IBM Plex: IBM Plex Mono where it used a monospace font, IBM Plex Sans for the rest. Chart axes show their numbers in IBM Plex Mono. Numbers line up in columns.
- The UI is compact by default, with 13px text. The density chip in the top bar switches to comfortable, with larger text and more space, and the browser remembers the choice. If you had turned compact off with the old chip, the UI opens comfortable.
- Buttons and fields share one look across the UI. A dispatch's Delete button is red.
- The zone, theme and density chips in the top bar can be reached with Tab and used with Enter or Space. A control with keyboard focus shows an outline.
- An error message now stays on screen until you close it, and it shows above an open dialog. The same message twice shows once, with a count.
- When the UI cannot reach the server, a banner under the top bar says so. It goes away by itself once the server answers again.
- A failed create or import now shows next to the form it came from, which stays open so you can fix the input and try again. A refused import no longer closes the import dialog and opens it again.
- The UI's own errors appear in the logs panel too, starting with `ui:`.
- The top of the UI has two bars. The top bar is for the whole site: the microgrid list, the system status, the Logs, Defaults, Report and help buttons, and the theme, density and zone chips.
- The bar under it is for the microgrid you opened: Topology and Dispatches, the Metrics, Formulas and Weather buttons, and the Python export. The Logs button is on every view.
- A panel opens docked unless you have floated it out of a strip: the logs along the bottom, the other panels down the right, and the canvas narrows to make room. Float a docked panel with its ⤒ button and it opens floating from then on.
- A floating panel's corner gripper resizes it sideways too. Like the height, the width you set stays after a reload, and after you dock the panel and float it again.
- The UI works in a window down to 1024px wide. The bars wrap onto more rows, and in a narrow window the panels docked on the right get narrower, down to their smallest width, so the canvas keeps about 600px.

## Bug Fixes

- The dispatch form read its start time in the browser's zone, while the dispatch list shows it in the zone the zone chip picks. The form now reads it in that zone too, and names the zone next to the field.
- The UI did not start in a browser that blocks site storage. It now starts and works; it just does not remember your settings.
- Dragging the corner gripper down on a floating Logs card that opened along the bottom made the card grow upward, even after you had moved it. Its top edge moved, and the corner you held stayed where it was. Now, while you drag, the corner you hold follows the pointer and the card's top-left corner stays in place.
- A card you resize past the right or bottom edge of the area the panels float in stops at that edge when you let go. If it already reached past that edge when you took hold of it, it can get smaller there, but no bigger.
