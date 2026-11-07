# Names and units

Every number in macrocosim has one unit, and that unit is in its name.
The same quantity has the same name in the topology document, HTTP, the
WebSocket, macroctl and Python. This page lists the rules and then the
name of each quantity on each surface. In the tables, "JSON" is the
topology document a script's microgrids become
([python-scripts.md](python-scripts.md)), and "HTTP / JSON" the bodies
of the HTTP routes.

## Rules

1. **Units.** Power is W, energy Wh, reactive power VAr, apparent power
   VA, voltage V, current A, frequency Hz. Durations are seconds, and
   fractions are allowed. Percent is 0 to 100, and state of charge is
   a percent too. There are two exceptions: boiler pressure stays in
   bar, and steam mass flow is kg/s.
2. **The unit is in the name of every plain number.** Topology and
   HTTP fields end in `_w`, `_wh`, `_var`, `_va`, `_v`, `_a`, `_hz`,
   `_pct`, `_bar` or `_s`. macroctl flags look like `--lifetime-s` and
   `--lower-w`. Rates end in `_w_per_s`, `_var_per_s` or `_kg_per_s`
   (`-per-s` in macroctl flags). A `%` is spelled `_pct`.
3. **Python arguments are typed and have no suffix.** They take
   `Power`, `Energy`, `Percentage`, `ReactivePower`, `ApparentPower`,
   `Voltage`, `Current` or `timedelta`. A bare number raises
   `TypeError`. Where `frequenz-quantities` has no type (ramp rates),
   the argument stays a float and keeps a suffix, such as
   `ramp_rate_w_per_s`. A topology field is the Python argument name
   plus its unit suffix: `initial_soc` is `initial_soc_pct`.
4. **Points in time are RFC 3339 strings with no unit suffix.** The
   time of an event ends in `_at` (`created_at`, `plugged_at`). A
   window is `start` and `end`. The time of a sample or a record is
   `ts`. The one exception is chart series: a component history's
   `samples` pairs and the `/metrics/history` arrays carry epoch
   seconds as numbers (`t_s`), because they are bulk data.
5. **Times of day** (`sunrise`, `sunset`) are an `"HH:MM"` string.
   They are UTC.
6. **Ids.** The id of a thing is `id`. A reference to another thing is
   spelled out: `microgrid_id`, `component_id`. A name that says what
   the id is for keeps its name: `collision_id`, `suggested_id`,
   `dispatch_id`, `--from`, `--to`. URL placeholders (`{mg}`, `{id}`)
   stay as they are.
7. **Values without a unit keep their names.** This covers ratios,
   power factor, counts, ports, enums and seeds: `reactive_pf_limit`,
   `power_factor`, `phases`, `grpc_port`, `seed`. Model settings carry
   their unit in the name too: `sigma_hz_per_sqrt_s`,
   `mean_rev_rate_per_s`.
8. **A generic `value` field has a `unit` field beside it.** This is
   for fields whose unit depends on the context: knob values, setpoint
   values and samples. The unit strings are `"W"`, `"Wh"`, `"VAr"`,
   `"VA"`, `"V"`, `"A"`, `"Hz"`, `"%"`, `"s"`, plus `"kg/s"` and
   `"bar"` for the steam boiler. The reactive unit is written `"VAr"`. The one exception is
   `/metrics/history`: its entries are `{t_s, value}` with no unit,
   because the stream name fixes the unit. `/metrics/latest` carries
   the `unit` of each stream.
9. **Names of things stay.** Stream and metric names (`grid_power`,
   `battery_pool_energy`) and route names (`drive`, `sample-lag`) do
   not carry a unit.
10. **Display.** The tables macroctl prints may scale to kW, kWh or MW.
    They print the unit of electrical metrics, SoC and humidity.
    Ratios (power factor, THD), temperature, wind, pressure,
    irradiance and an unspecified metric print none.
    Everything a program reads follows rule 1.

An unknown field in a JSON request body answers 422 with a JSON error
that names the field. An unknown field in a query string answers 400.
The WebSocket, the endpoints file, macroctl and Python accept no old
names.

## Power

| Quantity | JSON | HTTP / JSON | macroctl | Python | Unit |
|---|---|---|---|---|---|
| Rated band | `rated_lower_w`, `rated_upper_w` | | `list` columns `rated_lower_w`, `rated_upper_w` | `rated_lower`, `rated_upper` (`Power`) | W |
| Meter power | `power_w` | `power_w` in `drive`; knob `meter-power` | | `power: Power` | W |
| Setpoint | | `setpoints` entries carry `value` and `unit` | `set-power ID POWER_W` | | W |
| Bounds | | `envelope.active_w` as `[lower, upper]` | `augment-bounds --lower-w --upper-w` | | W |
| PV array size | `array_peak_w` | | | `array_peak: Power` | W |
| Recording peak | | `peak_grid_w` in the recording report | | `RecordingReport.peak_grid_w` | W |

## Energy

| Quantity | JSON | HTTP / JSON | macroctl | Python | Unit |
|---|---|---|---|---|---|
| Battery capacity | `capacity_wh` | | | `capacity: Energy` | Wh |
| Car capacity | | `capacity_wh` in `.../component/{id}/ev` | | `capacity` in `EvCharger.plug` (`Energy`) | Wh |
| Car energy | | `energy_wh` in `.../ev` | | | Wh |
| Boiler thermal mass | `capacity_wh_per_bar` | | | `capacity_wh_per_bar` (float) | Wh/bar |
| Boiler heat per kg | `wh_per_kg` | | | `wh_per_kg` (float) | Wh/kg |
| Recording totals | | `total_battery_charged_wh`, `total_battery_discharged_wh`, `total_pv_produced_wh`, `charge_wh`, `discharge_wh`, `produced_wh` | | the same names on `RecordingReport` | Wh |
| Energy streams | | `grid_energy`, `battery_pool_energy` (stream names) in `metrics/latest` | | the `Microgrid` properties `grid_energy`, `pv_energy`, `consumer_energy`, `battery_energy` (`Energy` signals) | Wh |

## Reactive and apparent power

| Quantity | JSON | HTTP / JSON | macroctl | Python | Unit |
|---|---|---|---|---|---|
| Meter reactive power | `reactive_power_var` | `reactive_power_var` in `drive` | | `reactive_power: ReactivePower` | VAr |
| Reactive setpoint | | `setpoints` entries | `set-reactive-power ID POWER_VAR` | | VAr |
| Reactive bounds | | `envelope.reactive_var` | `augment-reactive-bounds --lower-var --upper-var` | | VAr |
| Apparent-power cap | `reactive_apparent_va` | knob `reactive-apparent-va` (unit `"VA"`) | | `reactive_apparent: ApparentPower` | VA |
| Reactive cap as a ratio | `reactive_pf_limit` | knob `reactive-pf-limit` | | `reactive_pf_limit` (float) | none |
| Power factor | `power_factor`, `leading` | `power_factor`, `leading` | | `power_factor` (float) | none |
| Recording peak | | `peak_grid_var` | | `RecordingReport.peak_grid_var` | VAr |
| Power factor at the reactive peak | | `site_pf_at_reactive_peak` | | `RecordingReport.site_pf_at_reactive_peak` | none |

## Voltage, current and frequency

| Quantity | JSON | HTTP / JSON | macroctl | Python | Unit |
|---|---|---|---|---|---|
| Battery voltage | `voltage_v` | | | `voltage: Voltage` | V |
| Grid fuse rating | `rated_fuse_current_a` | | `list` prints `fuse=N A` | `rated_fuse_current: Current` | A |
| Car current limit | | `max_current_a` in `.../ev` | | `max_current` in `EvCharger.plug` (`Current`) | A |
| Nominal frequency | | `nominal_hz` in `/api/frequency` | | `nominal` in `set_frequency_model` (`Frequency`) | Hz |
| Frequency now | | `hz` in `/api/frequency` | | `Microgrid.frequency` (`Frequency`) | Hz |

## Percent and state of charge

| Quantity | JSON | HTTP / JSON | macroctl | Python | Unit |
|---|---|---|---|---|---|
| Initial SoC | `initial_soc_pct` | | | `initial_soc: Percentage` | % |
| SoC window | `soc_lower_pct`, `soc_upper_pct`, `soc_protect_margin_pct` | | | `soc_lower`, `soc_upper`, `soc_protect_margin` (`Percentage`) | % |
| SoC now | | `soc_pct` in `drive` | | `soc` signal returns `Percentage` | % |
| Car SoC | | `soc_pct`, `target_soc_pct` in `.../ev` | | `soc`, `target_soc` in `EvCharger.plug` (`Percentage`) | % |
| Car taper | | `taper_start_pct`, `taper_floor_pct` in `.../ev` | | `taper_start`, `taper_floor` in `EvCharger.plug` (`Percentage`) | % |
| Sunlight | `sunlight_pct` | `sunlight_pct` | | `sunlight: Percentage` | % |
| Weather peak | | `peak_pct` | | `peak` in `set_weather` (`Percentage`) | % |
| Cloud depth | | `cloud_depth_pct`, `depth_pct` | | `cloud_depth` in `set_weather`, `depth` in `pass_cloud` (`Percentage`) | % |
| Clear-sky level | | `clear_sky_pct` | | | % |
| Stream jitter | `stream_jitter_pct` | | | `stream_jitter: Percentage` | % |
| Weather jitter | `weather_jitter_pct` | | | `weather_jitter: Percentage` | % |

## Durations

| Quantity | JSON | HTTP / JSON | macroctl | Python | Unit |
|---|---|---|---|---|---|
| Stream interval | `interval_s` | | | `interval: timedelta` | s |
| Command delay | `command_delay_s`, `reactive_command_delay_s` | | | `command_delay`, `reactive_command_delay` (`timedelta`) | s |
| Device delay | `device_delay_s` | | | `device_delay: timedelta` | s |
| Request lifetime | | `ttl_s` in setpoint events, `remaining_s` in the component state's `setpoints[]` | `--lifetime-s` (whole seconds) | | s |
| Default lifetimes | | | the binary's `--request-lifetime`, `--augment-lifetime` | | s |
| Physics tick | `physics_tick_s` (microgrid) | | | `physics_tick` (`timedelta`) on `make_microgrid` | s |
| Sample lag | | `sample_lag_s` in `sample-lag` | | `set_sample_lag` (`timedelta`) | s |
| PV weather lag | `weather_lag_s` | | | `weather_lag: timedelta` | s |
| Cloud timing | | `cloud_mean_gap_s` (0 means no ambient clouds), `cloud_duration_s`, `cloud_ramp_s`, `duration_s`, `ramp_s` | | `cloud_mean_gap`, `cloud_duration`, `cloud_ramp` in `set_weather`, `duration`, `ramp` in `pass_cloud` (`timedelta`) | s |
| Recording length | | `elapsed_s` in the recording report | | `RecordingReport.elapsed_s` (float) | s |
| Dispatch duration | | `duration_s` | `dispatch create --duration-s` | | s |
| History window | | `window_s` (query) | | | s |
| Polling | | | `dashboard --interval-s` | `timeout`, `hold_for` (`timedelta`) | s |
| Stepped run | | | `scenario run --step-s` (above 0, default 0.1), `--until-s` | | s |

## Points in time

| Quantity | JSON | HTTP / JSON | macroctl | Python | Unit |
|---|---|---|---|---|---|
| Dispatch window | | `start`, `end` | `dispatch create --start` | | RFC 3339 |
| Dispatch record | | `created_at`, `updated_at` | | | RFC 3339 |
| Sample time | | `ts` in `metrics/latest`, `/setpoints` events, recording events, logs and every WebSocket event that carries a time | | | RFC 3339 |
| Car plugged | | `plugged_at` in `.../ev` | | | RFC 3339 |
| Weather clock | | `now`, and `start`, `end` of each cloud in `events` | | | RFC 3339 |
| Chart series | | `t_s` in `metrics/history` entries, and the first number of each `samples` pair in a component history | | | epoch seconds |
| Times of day | | `sunrise`, `sunset` | | `sunrise`, `sunset` in `set_weather` (`datetime.time`) | `"HH:MM"` (UTC) |

## Rates

| Quantity | JSON | HTTP / JSON | macroctl | Python | Unit |
|---|---|---|---|---|---|
| Active ramp rate | `ramp_rate_w_per_s` | | | `ramp_rate_w_per_s` (float) | W/s |
| Reactive ramp rate | `reactive_ramp_rate_var_per_s` | | | `reactive_ramp_rate_var_per_s` (float) | VAr/s |
| Frequency model | | `mean_rev_rate_per_s`, `sigma_hz_per_sqrt_s` in `/api/frequency` | | `mean_rev_rate`, `sigma` (float) in `set_frequency_model` | model settings |

## Steam and pressure

| Quantity | JSON | HTTP / JSON | macroctl | Python | Unit |
|---|---|---|---|---|---|
| Steam demand | `demand_kg_per_s` | `steam_demand_kg_per_s` in `drive`; knob `boiler-demand` (unit `"kg/s"`) | | `demand` (`MassFlow`) | kg/s |
| Pressure | `target_bar`, `max_bar`, `initial_bar` | `pressure_bar` in `drive`, `pressure_target_bar`; knob `boiler-pressure` (unit `"bar"`) | | `target`, `max`, `initial`, the `pressure` signal (`Pressure`) | bar |

## Modes and fault knobs

| Quantity | JSON | HTTP / JSON | macroctl | Python | Unit |
|---|---|---|---|---|---|
| Operational mode | `operational_mode` (`"inactive"`, `"telemetry_only"`, `"control_only"`, `"control_and_telemetry"`) | `operational_mode` in `runtime` and `topology` | | `operational_mode: OperationalMode` | none |
| Health | `health` | `health` in `status` and `topology` | | `health: Health` in a spec; the `health` knob of a live component | none |
| Telemetry mode | `telemetry_mode` | `telemetry_mode` in `status` and `topology` | | `telemetry_mode: TelemetryMode`; the `telemetry_mode` knob | none |
| Command mode | `command_mode` | `command_mode` in `status` and `topology` | | `command_mode: CommandMode`; the `command_mode` knob | none |

## Signs

| Value | Positive | Negative |
|---|---|---|
| Meter power | the subtree consumes | the subtree exports |
| Battery inverter power, `battery_power` | charging | discharging |
| Solar inverter power, `pv_power` | never (about 0 at night) | producing |
| `grid_power` | import | export |
| `consumer_power` | consuming | not used |
| Each `*_energy` | the integral of its power since the loopback's first sample, with the same sign | |

The aggregate streams `battery_pool_power` and `battery_pool_energy` are an
exception to "the same name on every surface": Python names them `battery_power`
and `battery_energy`.

## Ids

| Quantity | JSON | HTTP / JSON | macroctl | Python | Unit |
|---|---|---|---|---|---|
| Own id | `id` | `id` | `snapshot load --id` | `Component.id` | |
| Microgrid | `id` of a microgrid | `microgrid_id` on every WebSocket event about one microgrid | `--microgrid-id` | `Microgrid.id` | |
| Component | `connections` pairs (parent, child) | `component_id` in history, setpoints and WebSocket events; `component_ids` (comma-separated) in the formula query | `list --component-id` | | |
| Dispatch | | `dispatch_id` | `dispatch get ID` | | |
| Load collision | | `collision_id`, `suggested_id` | | | |
| Graph edges | | | `connections --from`, `--to` | | |
| gRPC address | | `grpc_addr` | | `Microgrid.grpc_url` (`grpc://` plus the address) | |

A microgrid in the endpoints file is `{"id", "name", "grpc_addr"}`.
The import and snapshot-load request bodies name the new microgrid
`id`.

