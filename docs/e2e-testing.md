# End-to-end testing your app against macrocosim in CI

Macrocosim is a high-fidelity stand-in for a real microgrid: it speaks the
production Frequenz gRPC API (`Microgrid`, `PlatformAssets`,
`MicrogridDispatchService`) and runs real component physics, while Python code
drives the environment (load, PV, faults) and checks the result. That makes it
a drop-in environment for a downstream control app's integration tests: your
app talks to macrocosim exactly as it would to a real deployment.

Macrocosim runs as a **separate process**, and your app connects over gRPC.
There is nothing to link or vendor: `pip install macrocosim` installs the
Python client together with the `macrocosim` and `macroctl` binaries.

## Two ways to test

Both run in real time. 0.1's deterministic, serverless gate
(`macroctl scenario run --stepped`) is gone, with Lisp and the `macroctl
scenario` commands: there is no stepped clock any more.

### 1. From pytest

A test launches a private simulator for its microgrid with `mc.launch`, on
free ports, starts your app against `microgrid.grpc_url`, drives the
environment through the microgrid's knobs and asserts with `expect`:

```python
async def test_import_stays_under_the_cap(launch_microgrid) -> None:
    microgrid = await launch_microgrid(SPEC)
    app = await start_my_app(microgrid.grpc_url)  # your app, your fixture
    await microgrid[LOAD].power.set(Power.from_kilowatts(60))
    await microgrid.grid_power.expect(
        mc.at_most(Power.from_kilowatts(40)),
        timeout=timedelta(seconds=30),
        hold_for=timedelta(seconds=10),
    )
```

Each test gets its own simulator, so tests do not share state. The `launch_microgrid` fixture and the rest of the
client are in the [Python README](../python/README.md#fixtures). A failing
`expect` raises `ExpectationFailed`, and the simulator's log stays in the
test's `tmp_path`.

### 2. From a script

A [script](python-scripts.md) is a plain program: its `main` connects to a
running simulator, declares the topology, drives the environment and asserts
with `expect`. Its exit status is the verdict: 0 when `main` returned, not 0
when it raised, for example on a failed `expect`.

```sh
# 1. boot the simulator; ephemeral ports keep parallel CI jobs from clashing,
#    and the endpoints file is the readiness signal.
macrocosim --ephemeral-ports --emit-endpoints=endpoints.json &
SIM=$!
# wait until bound; bail out if the simulator died at boot instead
# of hanging the job until CI's global timeout.
until [ -s endpoints.json ]; do kill -0 $SIM 2>/dev/null || exit 1; sleep 0.1; done
UI=$(jq -r '.ui' endpoints.json)                          # e.g. 127.0.0.1:33565

# 2. run the script against it, and wait until it declared microgrid 9.
MACROCOSIM_URL="http://$UI" python plant.py &
SCRIPT=$!
until GRPC=$(curl -s "http://$UI/api/microgrids" \
    | jq -er '.[] | select(.id == 9 and .runtime.status == "running")
              | .runtime.grpc_addr'); do
  kill -0 $SCRIPT 2>/dev/null || exit 1; sleep 0.1
done                                                      # e.g. [::1]:41979

# 3. boot YOUR app against the microgrid's gRPC address.
my-control-app --microgrid "grpc://$GRPC" &
APP=$!

# 4. the script's exit status is the verdict.
wait $SCRIPT; RC=$?
kill $APP $SIM
exit $RC
```

The script starts driving as soon as its microgrids are declared, so a script
that needs your app connected first waits for it, for example by expecting the
app's effect on a reading. A script that records with `csv=True` writes its CSV
files under `recordings/` in the state directory (`--state-dir`, default the
current directory); upload them, the script's output and your app's logs as CI
artifacts on failure.

`--emit-endpoints` prints one JSON line of the resolved addresses, to a file (as
above) or to stdout if given no path. It lists the microgrids the simulator
booted with (`--topology`, below); ones declared later are in `GET
/api/microgrids`. A file from a `--topology` boot:

```json
{"ui":"127.0.0.1:33565","microgrids":[{"id":9,"name":"demo","grpc_addr":"[::1]:41979"}],"assets":"[::1]:33881","dispatch":"[::1]:43253","version":"0.2.0"}
```

`--topology graph.json` boots the microgrids of a topology document, for an app
test that needs only a static site; they are in the endpoints file.

## Structuring fixtures

A test case is a **(graph, scenario, app-config)** triple. Keep these
fixtures in *your* repo: macrocosim is a generic engine that runs them.

- **One module per graph.** Define the topology once as `make_*` specs, and
  every scenario that exercises it as an async function that takes the
  microgrid. Tests import the specs and the scenarios; a script imports them
  too, and runs one from its `main`.

- **Parallelise at the test or CI-job level.** Each `mc.launch`, and each
  `macrocosim` run with `--ephemeral-ports`, is an isolated simulator on its
  own ports. This gives true isolation, independent retry, and parallelism
  over the real-time runs.

- **Several microgrids in one script** is for a single test case that is
  *genuinely* multi-microgrid (a fleet). It is **not** the way to parallelise
  independent cases: they would share one simulator, and its grid frequency.

## Determinism

Runs are **real time**: your app is a black box on its own clock. So assert
**settled-state invariants with tolerance** (for example "import held at or
under the cap, give or take a margin, for 10 s after the perturbation") with
`expect(..., hold_for=...)`, not exact transient values. Configure components
with zero command delay so steady state is reached quickly, and seed anything
random (`random.Random(seed)`, the weather's `seed=`).

## Reference

- `macrocosim`: `--topology PATH` · `--ui-port N` · `--ephemeral-ports` ·
  `--emit-endpoints[=PATH]` · `--state-dir DIR` · `--python PATH`; `macrocosim
  --help` lists the rest.
- `GET /api/microgrids`: every loaded microgrid, with its runtime's `status` and
  `grpc_addr`; the address serves while `status` is `running`.
- `macroctl --ui-addr … [--microgrid-id N]`: the readouts (`list`,
  `stream`, `pool`, `dashboard`) and commands (`set-power`,
  `augment-bounds`, `dispatch`), for a look at the running simulator.
- [`python-scripts.md`](python-scripts.md): scripts, re-runs and recording.
