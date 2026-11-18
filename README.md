# macrocosim

A microgrid simulator for testing downstream control apps. Components
(grid, meter, battery, inverters, EV charger, CHP, steam boiler — a
controllable gas/electric hybrid) are Rust types behind a single
`SimulatedComponent` trait. A Python script declares the topology and
animates it, with the same `macrocosim` client that integration tests
use.

The simulator exposes three surfaces:

- **gRPC** — Frequenz `Microgrid` v1alpha18 API. One binary can
  serve many microgrids; the first defaults to `[::1]:8800` and
  subsequent ones step by ten (`:8810`, `:8820`, …), or pin an
  explicit port with `grpc_port` on `make_microgrid(...)`.
  Downstream apps written against the production API talk to
  macrocosim the same way they'd talk to a real microgrid.
  Two enterprise-wide services ride alongside on their own
  sockets: `PlatformAssetsService` (`[::1]:9900`) and the
  `MicrogridDispatchService` store-and-serve dispatch API
  (`[::1]:8900`), each keyed by `microgrid_id` per request.
- **Web UI** (`http://127.0.0.1:8801`) — multi-microgrid SPA.
  Each microgrid gets a topology canvas (edit with undo / redo,
  pick a layout, align nodes), a per-component chart dashboard,
  and a *Formulas* tab. The Formulas tab shows every generated
  formula and the engine's reason for each part. The UI can
  also import a microgrid API site export as a real simulated
  microgrid, and shows recordings in its Report panel. Raw JS / HTML / CSS embedded
  into the binary via `rust-embed`; no build step.
- **macroctl** — clap-based client that drives both surfaces from the
  shell.

## Build & run

```sh
cargo build
cd python && uv sync && cd ..       # the Python client scripts import
cargo run --bin macrocosim -- --python python/.venv/bin/python
python/.venv/bin/python examples/assets/ev_charging.py   # in another terminal
```

The binary boots bare: UI up, no microgrids. A script is a separate Python
program that connects to it and declares its microgrids.
[`examples/`](examples/README.md) has short scripts, one feature each: assets,
testing an app, faults, and sites and the grid. Run a script again after an
edit; the microgrid stays loaded between runs, and an unchanged spec leaves it
as it is. Microgrids can also be made in the UI. `--topology topology.json`
boots microgrids from a topology document; that is how the Python client's
`launch` starts a simulator for a test.

Recordings are written under `--state-dir`, which defaults to the directory the
server was started from. A `.lisp` argument, or a state directory that still
holds 0.1's `enterprise.lisp` or `microgrids/*.lisp`, stops the binary with a
message naming the files: port them to a Python script, or pass another
`--state-dir`. A `.py` argument stops it too: run the script with `python`
instead. `GET /api/defaults` lists the per-category defaults a spec's missing
fields take.

The proto roots ([frequenz-api-microgrid](https://github.com/frequenz-floss/frequenz-api-microgrid),
frequenz-api-assets, frequenz-api-dispatch) are vendored as git
submodules under `submodules/` — run `git submodule update --init --recursive`
once after cloning. `MACROCOSIM_PROTO_ROOT` overrides the microgrid
proto root for downstream packagers with a private mirror; frequenz-api-common
still comes from the assets submodule.

## Scripting

A script is a plain program: it connects to the simulator, declares its
microgrids, and animates them:

```python
import asyncio

import macrocosim as mc
from frequenz.quantities import Power

load = mc.make_meter(id=2)
MICROGRID = mc.make_microgrid(id=7, topology=mc.make_grid_connection_point(id=1, successors=[load]))


async def main() -> None:
    async with mc.connect() as sim:
        plant = await sim.declare(MICROGRID)
        async with plant.record("first load", csv=True):
            await plant[load].power.set(Power.from_kilowatts(20))
            ...


if __name__ == "__main__":
    asyncio.run(main())
```

A scenario is an async function: stress events (load spikes, cloud cover,
outages, silent components) wrapped in a recording, which measures peak grid
power, battery charge and discharge, SoC statistics and 15-minute averages.
[`examples/testing/test_recording.py`](examples/testing/test_recording.py)
records a load step and checks the report.

[`docs/python-scripts.md`](docs/python-scripts.md) covers the script shape, what
a re-run keeps, recording, attaching a terminal with `macrocosim.connect`, and
porting a Lisp site file.

## macroctl

```sh
macroctl info
macroctl tree
macroctl list --category battery
macroctl connections --from 4                                  # filter graph edges
macroctl stream 1001 --samples 5
macroctl set-power 1001 -5000 --lifetime-s 30                  # watts; negative = discharge
macroctl set-reactive-power 1001 2000                          # VAr
macroctl augment-bounds 1001 --lower-w -1000 --upper-w 5000    # TTL-limited bounds
macroctl augment-reactive-bounds 1001 --lower-var -500 --upper-var 500
macroctl pool battery                                          # loopback BatteryPool snapshot
macroctl dashboard --tail                                      # one-line/sec pulse bar
macroctl dispatch list                                         # dispatch API CRUD
macroctl dispatch create <type> battery --duration-s 3600
macroctl --microgrid-id 2 info                                 # pick another microgrid
macroctl from-assets 42 -o site_42.py                          # a site from an Assets API
```

`--microgrid-id` selects the microgrid for the gRPC commands,
`dispatch`, `pool` and `dashboard`; the default is the lowest id.
`--addr` names the gRPC server directly; with `--microgrid-id` the
lookup still runs and the two must agree. `--ui-addr` (default
`http://127.0.0.1:8801`) points at the UI server, which the
HTTP-driven verbs (`dashboard`, `pool`) talk to. The gRPC commands
ask it for the microgrid's address unless `--addr` alone is given,
and `dispatch` asks it for the lowest microgrid id when
`--microgrid-id` is not given. `--json` swaps any human table for the
raw JSON.

`from-assets MICROGRID_ID` talks to an Assets API instead of a simulator. It
fetches the microgrid's components and connections, and writes a Python module
that declares the microgrid, as *Save as .py* does. The module goes to stdout,
or to the `-o` file. With `--json` it writes the topology document instead, and
needs no Python. `MICROGRID_ID` is the Assets API's id; the global
`--microgrid-id` is refused here.

The API's URL, key and signing secret come from `--url`, `--auth-key` and
`--sign-secret`, or from `$ASSETS_API_URL`, `$ASSETS_API_AUTH_KEY` and
`$ASSETS_API_SIGN_SECRET`. Prefer the environment variables, and set them
without typing the secret at the prompt, for example by sourcing a file only
you can read: a flag shows to every user in `ps`, and what you type can land in
the shell's history.

The URL is `grpc://host[:port]`, and the port defaults to 443. `?ssl=false` is
for a plaintext server, such as the simulator's own: it sends the key
unencrypted.

The module keeps the API's microgrid and component ids, and the fields the site
import keeps: names, rated bounds, battery capacity and SoC bounds, the fuse
current and operational modes.

Components outside their operational lifetime are left out, with their
connections, and so are connections outside their own. A module has exactly one
root, the grid connection point: a microgrid with more than one is refused. A
component that is not under the grid connection point, over the connections
that are left, is left out too.

Components the simulator has no model for are left out, such as a precharger or
an HVAC. Each parent of one of these is connected to each of its children, or,
when a child is left out too, to that child's children.

A hybrid inverter, and an inverter of no type over a battery, are left out with
the components only such inverters lead to, such as their batteries. An
inverter of no type is over a battery when the battery is its child, or when
only components the simulator has no model for are in between. A component
under two of these inverters goes with the first.

The module writes them as commented-out lines. Uncomment a hybrid inverter once
the simulator can model one. An inverter of no type is written as a battery
inverter: uncomment it once you know it is one.

They go in the `successors` of the first of the inverter's parents, in the API's
connection order, that is left and can have components under it: the grid
connection point, a meter, a breaker, a power transformer or a battery inverter.
An inverter with no such parent is not written, and `--json` carries none of
them.

stderr names every component left out, and why.

Recordings and scenarios run inside the script (see
[`docs/python-scripts.md`](docs/python-scripts.md)); macroctl has no
scenario or snapshot commands.

## Configuration knobs

- **Main / point-of-common-coupling meter** — derived from the
  topology, not flagged: it's the grid connection point's sole child
  when that child is a meter. A recording tracks its peak.
- **Knobs** — a spec sets each knob's starting value (`power` on a
  meter, `sunlight` on a solar inverter, `health`, the modes); a write
  from a script, a test or the UI changes the live value until the
  next write. A script changes them over time with ordinary asyncio
  loops.

## Architecture in one paragraph

A `MicrogridSite` owns one microgrid's component registry, physics
tick loop, telemetry-history rings, and recording journal; an
enterprise `microgrids` registry keys those sites by id so one
binary can serve many at once. A microgrid is built from a JSON
topology document; a script's only jobs are declaring that topology
and animating the environment over the HTTP API — every component's
tick / ramp / SoC stays in Rust. Inverter and battery share
only an electrical coupling: the battery's BMS clamps DC ingress,
the inverter publishes the measured aggregate, and a server-side
gateway intersects bounds for setpoint validation.

## More

- [`docs/e2e-testing.md`](docs/e2e-testing.md) — drive macrocosim from a
  downstream app's CI for end-to-end integration tests.
- [`docs/python-scripts.md`](docs/python-scripts.md) — scripts, re-runs,
  recording, and porting from Lisp.
- [`docs/names-and-units.md`](docs/names-and-units.md) — the unit and the
  name of every quantity in the topology document, HTTP, macroctl and
  Python.
- [`AGENTS.md`](AGENTS.md) — developer notes for this repo.
- [`todo.org`](todo.org) — roadmap + open design questions.

## License

The simulator (this crate and the `macrocosim` / `macroctl` binaries) is
[GPL-3.0-only](LICENSE). The Python client under [`python/`](python/) is
[MIT](python/LICENSE-MIT); its wheel bundles the GPL binaries and ships their
license alongside.

---

This project was previously called `switchyard`.
