# Scenarios

A scenario is a Lisp file that drives switchyard through a stress
test — sudden load spikes, cloud cover, battery outages, silent
components — while a Rust **reporter** records metrics and a
**journal** records named events. At any time you can ask the
running simulator what's happened so far via
`GET /api/scenario/report` or the **Report** panel in the UI.

The framework is two layers:

- **Driver** (the script you write) — schedules events using
  `(every …)` and `(run-with-timer …)` plus a small set of
  scenario-specific defuns. See `examples/scenario-driving.lisp`.
- **Reporter** (Rust observer, always running) — accumulates peak
  main-meter power, battery charge / discharge integrals, PV
  produced energy, SoC stats, and 15-minute window averages.
  Resets on `scenario-start`; freezes on `scenario-stop`.

## Loading a scenario

There's no auto-discovery. Load explicitly from the REPL —

```lisp
(load "examples/scenario-driving.lisp")
```

— or put the forms in the world script itself so they run at load
time (`examples/berlin-demo.lisp` registers its seven
`define-scenario`s that way). `(load …)` resolves relative to the
server's state dir (`--state-dir`, default: the directory the
server was started from).

## Lifecycle defuns

| Defun                              | Purpose                                          |
|------------------------------------|--------------------------------------------------|
| `(scenario-start NAME)`            | begin a run — clears the journal + reporters    |
| `(scenario-stop)`                  | end the run — freezes elapsed + metrics + CSV   |
| `(scenario-event KIND PAYLOAD)`    | append a journaled event                        |
| `(scenario-elapsed)`               | wall-clock seconds since start (frozen on stop) |
| `(scenario-end-after MINUTES)`     | schedule `(scenario-stop)` after MINUTES        |
| `(scenario-record-csv DIR)`        | start writing one CSV per component to DIR     |
| `(scenario-stop-csv)`              | close all CSV sinks (also implicit on stop)    |

`KIND` accepts a symbol or a string. `PAYLOAD` accepts any value
— it renders through Display when stored.

## Driving the environment

These setters work outside scenarios too — they're the same
animation knobs `examples/berlin-demo.lisp` uses for its built-in
load and cloud curves — but inside a scenario they're how a script
exercises the simulator:

| Defun                                  | Effect                                                   |
|----------------------------------------|----------------------------------------------------------|
| `(set-meter-power ID VAL)`             | drive a meter's `:power` (number / lambda / `'symbol`)   |
| `(set-solar-sunlight ID VAL)`          | drive a solar inverter's `:sunlight%` (same polymorphism)|
| `(set-component-health ID K)`          | flip health to `'ok` / `'error` / `'standby`             |
| `(set-component-telemetry-mode ID K)`  | `'normal` / `'silent` / `'closed`                        |
| `(set-component-command-mode ID K)`    | `'normal` / `'timeout` / `'error`                        |
| `(set-active-power ID W &OPTIONAL MS CLAMP)` | gRPC-style setpoint; MS = lifetime in ms, non-nil CLAMP clamps into the live envelope instead of rejecting |
| `(set-reactive-power ID VAR &OPTIONAL MS CLAMP)` | same for the reactive axis; CLAMP pulls into `reactive_setpoint_envelope` (own PF / kVA band ∩ children's Q bands ∩ live augmentations), falling back to the component's own band when no child reports one |

`(set-meter-power 100 (lambda () (csv-lookup …)))` and
`(set-meter-power 100 'consumer-power)` install the lambda or
the symbol as the source — the scheduler re-resolves it once per
tick. An imperative numeric `set-meter-power` collapses any prior
dynamic source back to a constant.

`(set-component-telemetry-mode 200 'silent)` plus
`(set-component-command-mode 200 'timeout)` simulates a
"flaky network" — the inverter keeps producing power and the
physics keeps simulating, but the gRPC stream goes quiet and
SetPower requests hang. Useful for exercising downstream apps
that need to cope with stale or unresponsive sources.

## Helpers in `sim/scenarios.lisp`

These helpers are built into the binary. Switchyard evaluates the
`sim/*.lisp` preludes at startup, before your config runs, so no
explicit `(load …)` is needed:

- `(random-uniform LOW HIGH)` — uniform float in `[LOW, HIGH)`.
- `(random-pick LIST)` — one element of `LIST`, uniformly. `nil`
  on empty.
- `(random-outage IDS &rest opts)` — recurring random outages on
  a random pick from `IDS`. Plist opts: `:min-every` /
  `:max-every` (gap seconds), `:min-duration` / `:max-duration`
  (outage seconds), `:kind` (health symbol while down — default
  `'error`). Each transition lands as a journal event.

## Reading the report

The reporter exposes:

```sh
curl -s http://127.0.0.1:8801/api/scenario          # lifecycle
curl -s http://127.0.0.1:8801/api/scenario/events   # journal (paginated)
curl -s http://127.0.0.1:8801/api/scenario/report   # aggregate metrics
```

`/api/scenario/events` takes `?since=N&limit=M` for incremental
polling — pass back the previous response's `next_event_id`
unchanged.

`/api/scenario/report` returns:

| Field                          | Meaning                                                   |
|--------------------------------|-----------------------------------------------------------|
| `scenario_elapsed_s`           | seconds since `scenario-start`; frozen on stop            |
| `peak_main_meter_w`            | max active-power on the main meter so far                 |
| `main_meter_id`                | id of the main meter (derived from the topology — see below), or `null` |
| `total_battery_charged_wh`     | sum across batteries; positive DC power → charging        |
| `total_battery_discharged_wh`  | sum across batteries; negative DC power → discharging     |
| `total_pv_produced_wh`         | sum across solar inverters; negative active P → produced  |
| `per_battery`                  | `[{ id, charge_wh, discharge_wh }]`                       |
| `per_pv`                       | `[{ id, produced_wh }]`                                    |
| `soc_stats`                    | `{ mean_pct, median_pct, mode_pct }` over current SoCs   |
| `main_meter_window_averages`   | `[{ window_start, avg_w }]` — average main-meter power per 15-min UTC-aligned window, oldest first, ≤ 96 |
| `name`                         | name of the running scenario; `null` before any start     |
| `checks_passed`                | count of passed `(scenario-expect …)` checks, full run    |
| `checks_failed`                | count of failed checks, full run                          |
| `checks`                       | recent check results, oldest first (bounded ring)         |

Peak tracking needs a main / point-of-common-coupling meter, which
is derived from the topology: the grid connection point's sole child,
when that child is a meter.

```lisp
(make-grid-connection-point :id 1
  :successors (list (make-meter :id 2 :successors …)))
```

The sample `examples/berlin-demo.lisp` already has this shape. If the grid has no
child, more than one child, or a single non-meter child, there is no
unambiguous main meter and peak tracking stays idle.

## Recording CSVs

`(scenario-record-csv "csvs")` opens one buffered CSV per
component under `csvs/`, named `<id>-<category>.csv`, with a
uniform 5-column header (`ts_iso, active_power_w,
reactive_power_var, dc_power_w, soc_pct`) — empty cells where a
component doesn't publish that field. Rows write at the 1 Hz
history-sampler cadence.

Components with an active-power envelope (the ones a control app
commands) get two extra files:

- `<id>-setpoints.csv` — the control inputs the component
  received: one row per SetActivePower / SetReactivePower /
  AugmentBounds request (value, resolved TTL, arrival time,
  outcome). Written on each request, not sampled.
- `<id>-bounds.csv` — the effective active-power envelope over
  time, sampled at the same 1 Hz pass as telemetry.

`(scenario-stop)` flushes and closes all files;
`(scenario-stop-csv)` does the same on demand mid-scenario.
