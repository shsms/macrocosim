# switchyard

A microgrid simulator for testing downstream control apps. Components
(grid, meter, battery, inverters, EV charger, CHP) are Rust types
behind a single `SimulatedComponent` trait; the topology + animation
script is Lisp via [`tulisp`](https://github.com/shsms/tulisp).

The simulator exposes three surfaces:

- **gRPC** — Frequenz `Microgrid` v1alpha18 API. One binary can
  serve many microgrids; the first defaults to `[::1]:8800` and
  subsequent ones step by ten (`:8810`, `:8820`, …), or pin an
  explicit port with `:grpc-port` on `(make-microgrid …)`.
  Downstream apps written against the production API talk to
  switchyard the same way they'd talk to a real microgrid.
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
  microgrid, and run scenarios. Raw JS / HTML / CSS embedded
  into the binary via `rust-embed`; no build step.
- **swctl** — clap-based client that drives both surfaces from the
  shell.

## Build & run

```sh
cargo build
cargo run --bin switchyard examples/berlin-demo.lisp
```

The binary takes zero or more Lisp scripts. Each script is a
self-contained world — `examples/berlin-demo.lisp` wires a demo
topology, animates its AC environment, and registers seven starter
scenarios; saving the file hot-reloads the world. With no scripts
the engine boots bare: UI + REPL up, no microgrids, and you load a
script on demand from the Microgrids tab or the REPL:

```lisp
(load "examples/berlin-demo.lisp")
```

Relative paths (and all persistent state: managed microgrid files
under `microgrids/`, `enterprise.lisp`, `snapshots/`) anchor to
`--state-dir`, defaulting to the directory the server was started
from. See `sim/defaults.lisp` for the per-category default knobs and
`sim/common.lisp` for the runtime helpers — both are embedded into
the binary, so a script never needs to load them.

The proto roots ([frequenz-api-microgrid](https://github.com/frequenz-floss/frequenz-api-microgrid),
frequenz-api-assets, frequenz-api-dispatch) are vendored as git
submodules under `submodules/` — run `git submodule update --init --recursive`
once after cloning. `SWITCHYARD_PROTO_ROOT` overrides the microgrid
proto root for downstream packagers with a private mirror.

## Scenarios

A scenario is a Lisp script that drives the simulator through stress
events (load spikes, cloud cover, random outages, silent components)
while a Rust reporter records peak / charge / discharge / SoC stats
and per-15-minute averages. See [`scenarios/README.md`](scenarios/README.md)
for the framework, [`examples/scenario-driving.lisp`](examples/scenario-driving.lisp)
for a runnable 30-minute sample.

```sh
swctl scenario start "demo"
swctl scenario load examples/scenario-driving.lisp
swctl scenario report
swctl scenario events --since 0 --limit 20
swctl scenario stop
```

The Report panel in the web UI polls the same endpoints as `swctl
scenario report`.

## swctl

```sh
swctl info
swctl tree
swctl list --category battery
swctl connections --from 4                                  # filter graph edges
swctl stream 1001 --samples 5
swctl set-power 1001 -5000 --lifetime 30                    # negative = discharge
swctl augment-bounds 1001 --lower -1000 --upper 5000        # TTL-limited bounds
swctl pool battery                                          # loopback BatteryPool snapshot
swctl scenario report                                       # journal report / CI gate
swctl scenario list                                         # registered scenarios
swctl scenario run cloud-fade --wait --assert              # run one live + gate
swctl snapshot save before-test                             # freeze the mg's managed file
swctl dashboard --tail                                      # one-line/sec pulse bar
swctl dispatch list 1                                       # dispatch API CRUD
swctl dispatch create 1 <type> battery --duration 3600
```

`--addr` (default `http://[::1]:8800`) points the gRPC client
at the first microgrid; for additional microgrids pass
`--addr http://[::1]:8810` etc. `--ui-addr` (default
`http://127.0.0.1:8801`) points the HTTP-driven verbs
(`scenario*`, `snapshot`, `dashboard`). `--json` swaps any
human table for the raw JSON.

The `scenario` subcommand covers both the ad-hoc journal verbs
(start / stop / event / load / report / events / summary) and the
registered scenarios from `(define-scenario …)`: `list` them and
`run NAME` — live (`--wait`) or headless deterministic (`--stepped`),
with `--assert` to gate CI.

## Configuration knobs

- **Main / point-of-common-coupling meter** — derived from the
  topology, not flagged: it's the grid connection point's sole child
  when that child is a meter. The scenario reporter tracks its peak.
- **`(make-meter :power N | (lambda () …) | 'symbol)`** — drive the
  meter's published power from a constant, a lambda, or a global
  symbol. Same on solar inverters via `:sunlight%`.
- **`(set-meter-power id N | (lambda () …) | 'symbol)`** — same
  polymorphism imperatively, for `(every …)` callbacks or scenario
  scripts. Numeric values collapse any prior dynamic source.

## Architecture in one paragraph

A `MicrogridSite` owns one microgrid's component registry, physics
tick loop, telemetry-history rings, and scenario journal; an
enterprise `microgrids` registry keys those sites by id so one
binary can serve many at once. Lisp's only jobs are wiring topology
(`(make-grid)`, `(make-meter)`, … inside the `:topology` lambda of
`(make-microgrid …)`) and animating the environment (`(every …)`,
`(run-with-timer …)`, `(set-meter-power)`, etc.) — every component's
tick / ramp / SoC derate stays in Rust. Inverter and battery share
only an electrical coupling: the battery's BMS clamps DC ingress,
the inverter publishes the measured aggregate, and a server-side
gateway intersects bounds for setpoint validation.

## More

- [`docs/e2e-testing.md`](docs/e2e-testing.md) — drive switchyard from a
  downstream app's CI for end-to-end integration tests.
- [`AGENTS.md`](AGENTS.md) — developer notes for this repo.
- [`todo.org`](todo.org) — roadmap + open design questions.
- [`scenarios/README.md`](scenarios/README.md) — scenario framework
  reference.
