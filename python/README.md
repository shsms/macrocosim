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

def test_limiter_holds_import_cap():
    bat = sw.battery(id=4, capacity=Energy.from_kilowatt_hours(92),
                     soc=Percentage.from_percent(50))
    inv = sw.battery_inverter(id=3, successors=[bat],
        rated=(Power.from_kilowatts(-5), Power.from_kilowatts(5)))
    mg  = sw.Microgrid(id=1,
        topology=sw.grid(id=1, successors=[sw.meter(id=2, successors=[inv])]))

    with sw.launch(mg) as site:
        site.component(inv).status(health=sw.Health.ERROR)            # fault injection
        site.component(inv).expect.active_power(
            approx=Power.from_watts(0), tol=Power.from_watts(100))
        site.component(bat).expect.soc(
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
    site.grpc          # first microgrid's gRPC address
    site.eval("(...)") # raw Lisp escape hatch
```

**Read** — component telemetry over gRPC (what the app sees), aggregates
over the graph-derived formulas:

```python
site.active_power(3)      # Power | None       (component, gRPC)
site.soc(4)               # Percentage | None  (battery SoC, gRPC)
site.grid_power()         # Power | None       (/ pv_power() / consumer_power() / …)
```

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
microgrid aggregate (`site.expect`):

```python
site[3].expect.active_power(
    approx=Power.from_kilowatts(2), tol=Power.from_watts(300),
    timeout=timedelta(seconds=15))
site.expect.grid_power(
    max=Power.from_megawatts(1), for_=timedelta(seconds=30))
site[4].expect.soc(
    within=(Percentage.from_percent(45), Percentage.from_percent(55)))
```

`expect.<metric>(…)` polls until the matcher holds; pass `for_=` to require it
on every sample across a duration instead. Matchers: `approx`+`tol`, `within`,
`max`, `min`.

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

def test_grid_holds(switchyard):
    switchyard.expect.grid_power(
        approx=Power.from_kilowatts(7), tol=Power.from_watts(500))

@pytest.mark.switchyard_scenario("cloud-fade")   # runs + gates after the test
def test_scenario(switchyard): ...
```

## Status

Early but functional end to end — see `todo.org` §Y in the switchyard repo
for the design and roadmap. The public API is synchronous (pytest-first);
an async surface is a planned follow-up. Runnable `examples/` cover each
piece; `examples/pytest_demo/` is a live suite.
