# switchyard (Python)

Python integration-testing client for the
[switchyard](https://github.com/shsms/switchyard) microgrid simulator.

Build a topology, launch the simulator, drive its environment, inject
faults, and assert on the resulting grid state — from inside `pytest`,
talking to it the same way a downstream control app would (the gRPC
Microgrid API + the HTTP `/api` control plane).

```python
from datetime import timedelta
from frequenz.quantities import Energy, Percentage, Power
import switchyard as sw

async def test_limiter_holds_import_cap():
    bat = sw.battery(id=4, capacity=Energy.from_kilowatt_hours(92),
                     soc=Percentage.from_percent(50))
    inv = sw.battery_inverter(id=3, successors=[bat],
        rated=(Power.from_kilowatts(-5), Power.from_kilowatts(5)))
    mg  = sw.Microgrid(id=1,
        topology=sw.grid(id=1, successors=[sw.meter(id=2, successors=[inv])]))

    with sw.launch(mg) as site:
        site.component(inv).status(health=sw.Health.ERROR)            # fault injection
        await site.component(inv).expect.active_power(
            approx=Power.from_watts(0), tol=Power.from_watts(100))
        await site.component(bat).expect.soc(
            within=(Percentage.from_percent(45), Percentage.from_percent(55)))
```

## Install

Released as a **platform wheel** that bundles the `switchyard` + `swctl`
binaries (built from the Rust crate via maturin), so a downstream install
just works — no separate binary to fetch:

```sh
pip install 'switchyard[grpc]'      # or: uv add 'switchyard[grpc]'
```

`launch()` finds the bundled binaries automatically (in the interpreter's
scripts directory, even off PATH). Override with `SWITCHYARD_BIN` /
`SWCTL_BIN`, or `bin=` on `launch`, to point at a local build.

## Development

This package is [uv](https://docs.astral.sh/uv/)-native; the build backend
is maturin, so `uv sync` compiles the binaries from the repo's Rust crate
(needs the Rust toolchain + `git submodule update --init`).

```sh
uv sync                       # venv from uv.lock; builds the binaries
uv run pytest                 # tests; ruff / ty also via `uv run`
```

Point tests at a fast local `cargo` build instead of the wheel's binary:

```sh
SWITCHYARD_BIN=../target/debug/switchyard uv run pytest
```

## The surface

**Build** a topology — the spec *is* the graph (nested `successors`),
rendering 1:1 to switchyard's `(make-*)` Lisp:

```python
from frequenz.quantities import Energy, Percentage, Power

P = Power.from_kilowatts
mg = sw.Microgrid(id=1, topology=sw.grid(id=1, successors=[
    sw.meter(id=2, successors=[
        sw.battery_inverter(id=3, rated=(P(-5), P(5)), successors=[
            sw.battery(id=4, capacity=Energy.from_kilowatt_hours(100),
                       soc=Percentage.from_percent(50))]),
        sw.solar_inverter(id=5, sunlight=Percentage.from_percent(80)),
        sw.meter(id=6, power=Power.from_watts(1000))])]))
```

Constructors: `grid`, `meter`, `battery_inverter`, `solar_inverter`,
`battery`, `ev_charger`, `chp`. Kwargs mirror the plist keys
(`snake_case` → `:kebab-case`): `rated=(lo, hi)` `Power` bounds, `capacity`
an `Energy`, `soc` / `sunlight` a `Percentage`; `sw.raw("(lambda () …)")`
splices Lisp.

Typed throughout — no bare numbers or unit strings. Knobs take enums
(`sw.Health`, `sw.CommandMode`, `sw.TelemetryMode`, scenario `sw.Metric`,
`sw.Schedule`); quantities are
[`frequenz-quantities`](https://pypi.org/project/frequenz-quantities/)
(`Power`, `Energy`, `Percentage`, `Frequency`), imported from it directly;
times are `datetime` (`timedelta`, or a `datetime.time` for an absolute scenario clock).
`sw.to_lisp_atom(v)` shows the Lisp literal any of them emits.
Or use an existing config: `sw.launch("topology.lisp")`.

**Launch** and get a `Site` (a context manager; tears the process down):

```python
with sw.launch(mg) as site:
    site.grpc          # first microgrid's gRPC address ("host:port")
    site.grpc_url      # ... as a "grpc://host:port" URL a client connects with
    site.eval("(...)") # raw Lisp escape hatch
```

**Read** — component telemetry over gRPC (what the app sees), aggregates
over the graph-derived formulas:

```python
site.active_power(3)      # Power | None       (component, gRPC)
site.soc(4)               # Percentage | None  (battery SoC, gRPC)
site.grid_power()         # Power | None       (/ pv_power() / consumer_power() / …)
```

**Energy** — the simulator integrates each power aggregate into a cumulative
energy stream (server-side), so you can judge what an app *did* over a run:

```python
site.grid_energy()        # Energy | None  (cumulative, import positive)
site.battery_energy()     # Energy | None  (/ consumer_energy() / pv_energy())
```

Assert on them through `expect` (a one-shot check — energy accumulates, so it
isn't a settling value to poll):

```python
await site.expect.grid_energy(max=Energy.from_kilowatt_hours(15))   # held import down
await site.expect.battery_energy(approx=Energy.from_kilowatt_hours(-8),
                                 tol=Energy.from_kilowatt_hours(1))  # discharge total
```

Per-component energy is a first-class metric too, assertable from Lisp and the
scenario framework: `(check "15m" :component 2 :metric 'energy :max 15000.0)` or
`metric=sw.Metric.ENERGY` on a Python `Scenario.check`.

**Mutate** — reach a component with `site[id]` (or `site.component(id)`), then
act by *intent*:

```python
inv = site[3]
inv.command(active_power=Power.from_kilowatts(2),      # app command (gRPC gateway)
            lifetime=timedelta(seconds=30))
inv.command(bounds=(Power.from_kilowatts(-1),
                    Power.from_kilowatts(1)))           # narrow the envelope
inv.status(health=sw.Health.ERROR)                       # inject a fault
site[6].drive(power=Power.from_megawatts(2))            # drive the environment
site[5].drive(sunlight=Percentage.from_percent(30))
```

`command` goes through the real gRPC gateway, so an out-of-envelope value
raises `sw.SetpointRejected` (the production behaviour under test); `fault` /
`drive` are test-side stimuli.

**Assert** — settle-aware `expect`, on a component (`site[id].expect`) or a
microgrid aggregate (`site.expect`). The settle-aware assertions are `async`
(they `await` between polls, so an app under test on the same event loop keeps
running); the cumulative-energy ones are one-shot but `async` too, for a
uniform surface:

```python
await site[3].expect.active_power(
    approx=Power.from_kilowatts(2), tol=Power.from_watts(300),
    timeout=timedelta(seconds=15))
await site.expect.grid_power(
    max=Power.from_megawatts(1), for_=timedelta(seconds=30))
await site[4].expect.soc(
    within=(Percentage.from_percent(45), Percentage.from_percent(55)))
```

`await expect.<metric>(…)` polls until the matcher holds; pass `for_=` to
require it on every sample across a duration instead. Matchers: `approx`+`tol`,
`within`, `max`, `min`.

**Async core (v2)** — `switchyard.aio` is the async-native core: every
read, write, and wait is a coroutine on your event loop (no background
threads). Its assertion surface is generic-first: pass a metric from
`switchyard.metrics`, and the metric's *kind* picks the semantics
(power settles, energy is checked once):

```python
from switchyard.metrics import ACTIVE_POWER, BATTERY_ENERGY, GRID_POWER

async with sw.aio.launch(mg) as site:
    await site[5].drive(power=Power.from_kilowatts(20))
    await site.expect(GRID_POWER, max=Power.from_kilowatts(13))
    await site[3].expect(ACTIVE_POWER, approx=Power.from_kilowatts(2),
                         tol=Power.from_watts(300))
    await site.expect(BATTERY_ENERGY, max=Energy.from_watt_hours(-1))
```

`status()` and `drive()` constants go over typed JSON control endpoints
(`/api/component/{id}/status` / `/drive`); a rejection (unknown id, bad
value) raises `ControlRejected`. A `raw(...)` drive (lambda / symbol)
still rides `/api/eval`. See `docs/python-api-redesign.org` for the
design.

**Scenarios** — author in Python, or run a registered Lisp scenario:

```python
scn = sw.Scenario("cloud-fade", length=timedelta(minutes=4)).check(
    timedelta(seconds=110), component=2, metric=sw.Metric.ACTIVE_POWER,
    approx=Power.from_megawatts(1.5), tol=Power.from_kilowatts(300))
site.define_scenario(scn).run(wait=True).assert_passed()

# deterministic, serverless gate (no app under test):
sw.run_scenario_stepped([mg, scn], "cloud-fade")
```

**pytest** — the plugin auto-loads; provide a `switchyard_config` fixture:

```python
@pytest.fixture
def switchyard_config():
    return mg

async def test_grid_holds(switchyard):
    await switchyard.expect.grid_power(
        approx=Power.from_kilowatts(7), tol=Power.from_watts(500))

@pytest.mark.switchyard_scenario("cloud-fade")   # runs + gates after the test
def test_scenario(switchyard): ...
```

The `expect` assertions are `async`, so awaiting them needs `pytest-asyncio`
(installed with the `grpc` extra) and `asyncio_mode = "auto"` in your pytest
config — otherwise an `async def` test is collected but never awaited, and the
assertion silently never runs.

## Status

Early but functional end to end — see `todo.org` §Y in the switchyard repo
for the design and roadmap. Building, launching, reading and mutating are
synchronous; the settle-aware `expect` assertions are `async` (so they compose
with an app under test on the same event loop). Runnable `examples/` cover each
piece; `examples/pytest_demo/` is a live suite.
