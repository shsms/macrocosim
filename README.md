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
  sockets: `PlatformAssets` (`[::1]:9900`) and the
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
cd python && uv sync && cd ..       # the client the script child imports
cargo run --bin macrocosim -- --python python/.venv/bin/python \
    examples/starter_site.py
```

The binary takes one Python script and runs it in a child process.
`examples/starter_site.py` declares a demo microgrid, animates its
environment, and defines seven starter scenarios; saving the file
reloads the script without restarting the binary. With no script the
engine boots bare: UI up, no microgrids, and microgrids are made in
the UI. `--topology topology.json` boots microgrids from a topology
document instead, with no script; that is how the Python client's
`launch` starts a simulator for a test.

Recordings are written under `--state-dir`, which defaults to the
directory the server was started from. A `.lisp` argument, or a state
directory that still holds 0.1's `enterprise.lisp` or
`microgrids/*.lisp`, stops the binary with a message naming the files:
port them to a Python script, or pass another `--state-dir`.
`GET /api/defaults` lists the per-category defaults a spec's missing
fields take.

The proto roots ([frequenz-api-microgrid](https://github.com/frequenz-floss/frequenz-api-microgrid),
frequenz-api-assets, frequenz-api-dispatch) are vendored as git
submodules under `submodules/` — run `git submodule update --init --recursive`
once after cloning. `MACROCOSIM_PROTO_ROOT` overrides the microgrid
proto root for downstream packagers with a private mirror.

## Scripting

A script defines `MICROGRIDS`, a list of microgrid specs, and an
`async def run(microgrids)` that animates them:

```python
import macrocosim as mc
from frequenz.quantities import Power

load = mc.make_meter(id=2)
MICROGRIDS = [
    mc.make_microgrid(id=7, topology=mc.make_grid_connection_point(id=1, successors=[load])),
]


async def run(microgrids: dict[int, mc.Microgrid]) -> None:
    async with microgrids[7].record("first load", csv=True):
        await microgrids[7][load].power.set(Power.from_kilowatts(20))
        ...
```

A scenario is an async function: stress events (load spikes, cloud
cover, outages, silent components) wrapped in a recording, which
measures peak grid power, battery charge and discharge, SoC statistics
and 15-minute averages. [`examples/scenario_driving.py`](examples/scenario_driving.py)
is a 30-minute sample.

[`docs/python-scripts.md`](docs/python-scripts.md) covers the script
shape, what a reload keeps, recording, attaching a terminal with
`macrocosim.connect`, and porting a Lisp site file.

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
- [`docs/python-scripts.md`](docs/python-scripts.md) — scripts, reloads,
  recording, and porting from Lisp.
- [`docs/names-and-units.md`](docs/names-and-units.md) — the unit and the
  name of every quantity in the topology document, HTTP, macroctl and
  Python.
- [`AGENTS.md`](AGENTS.md) — developer notes for this repo.
- [`todo.org`](todo.org) — roadmap + open design questions.
- [`scenarios/README.md`](scenarios/README.md) — scenario framework
  reference.

## License

The simulator (this crate and the `macrocosim` / `macroctl` binaries) is
[GPL-3.0-only](LICENSE). The Python client under [`python/`](python/) is
[MIT](python/LICENSE-MIT); its wheel bundles the GPL binaries and ships their
license alongside.

---

This project was previously called `switchyard`.
