//! gRPC loopback supervisor that mirrors switchyard's own gRPC
//! service back through `frequenz-microgrid`'s client + logical-
//! meter actors. Every dashboard formula tile reads from there, so
//! the SPA exercises exactly the same path a downstream EMS would.
//!
//! `spawn_microgrid_loopback` kicks the supervisor task; the
//! supervisor watches `MicrogridSite` events and rebuilds the
//! `Microgrid` handle every time the topology changes (which also
//! resubscribes every forwarder against the new graph).

use std::time::Duration;

use frequenz_microgrid::{
    LogicalMeterConfig, LogicalMeterHandle, Microgrid, MicrogridClientHandle, Sample, metric,
    quantity::{Power, ReactivePower},
};
use tokio::sync::broadcast::error::RecvError;
use tokio::task::JoinHandle;

use crate::sim::{MicrogridSite, events::SiteEvent};

use super::state::{
    HistorySample, MICROGRID_HISTORY_CAP, MicrogridSampleSnapshot, SharedMicrogrid,
};

/// Spawn a tokio task that constructs a [`Microgrid`] pointed at
/// `grpc_url`, kicks off forwarders for the aggregated streams the
/// Dashboard cares about, and stores the handle in `slot` once the
/// connection succeeds. `Microgrid::try_new` already retries lazily
/// until the gRPC server is reachable; this wrapper exists so the
/// UI's `serve` doesn't block on the gRPC server coming up — UI
/// startup proceeds, and dashboard endpoints return 503 until the
/// slot fills.
///
/// `site` is the sink the forwarders publish to via
/// [`MicrogridSite::broadcast_microgrid_sample`]; the existing `/ws/events`
/// stream then carries the samples to the SPA without any extra
/// wiring — they ride the same `SiteEvent` discriminator the
/// per-component samples already use.
pub fn spawn_microgrid_loopback(grpc_url: String, slot: SharedMicrogrid, site: MicrogridSite) {
    tokio::spawn(async move {
        // Subscribe BEFORE the initial build. The build can take a
        // while (the graph build loops until it succeeds), and a
        // topology change landing in that window must queue an
        // event here — otherwise the loopback would serve the
        // pre-change graph until the NEXT mutation, silently.
        let events = site.subscribe_events();
        if build_microgrid(&grpc_url, &slot, &site).await {
            log::info!("microgrid loopback: connected + graph built + forwarders running");
        }
        // Watch for topology mutations and rebuild on each. The
        // graph crate's ComponentGraph is snapshotted at try_new
        // time so formulas + subscriptions go stale once the site
        // mutates; rebuilding picks up the new shape. Entered even
        // when the initial build failed: a transient failure (gRPC
        // transport hiccup) heals on the next topology event
        // instead of leaving the dashboard 503 forever.
        run_supervisor(grpc_url, slot, site, events).await;
    });
}

/// Build a fresh `Microgrid` and wire up its forwarders. Same
/// code path for the initial boot and every subsequent rebuild:
/// `slot.client` is lazily initialised on first call via
/// `MicrogridClientHandle::try_new(grpc_url)`, then reused
/// forever. Each call builds a fresh `LogicalMeterHandle` against
/// the current topology and assembles the `Microgrid` via
/// `new_from_handles`. The old `Microgrid` (replaced in `slot`)
/// drops normally; its `LogicalMeterActor` exits cleanly because
/// it handles a closed instructions channel by breaking out.
///
/// Forwarder subscriptions are awaited synchronously **before** the
/// slot swap. The shared `MicrogridClientActor` caches a
/// `broadcast::Sender` per component and its backing tonic stream
/// task exits the moment it sees `receiver_count == 0` between
/// upstream samples (tracked upstream in frequenz-microgrid-rs).
/// Subscribing the new LM first keeps that count ≥ 1 across the
/// handoff, so the stream task survives and samples reach the new
/// forwarders without a multi-second silence.
///
/// Returns false if the gRPC connect or graph build fails outright
/// (which the crate normally retries through; a hard failure means
/// something like a malformed URL).
async fn build_microgrid(grpc_url: &str, slot: &SharedMicrogrid, site: &MicrogridSite) -> bool {
    // Lazy client init. `MicrogridClientHandle::try_new` doesn't
    // contact the server — the connection is established lazily on
    // the first RPC — so this is cheap to call. It does validate
    // the URL though, hence the Result.
    let client = match slot
        .client
        .get_or_try_init(|| MicrogridClientHandle::try_new(grpc_url.to_owned()))
        .await
    {
        Ok(c) => c.clone(),
        Err(e) => {
            log::error!("microgrid loopback: client try_new failed: {e}");
            return false;
        }
    };
    // 1 Hz sample cadence matches the existing history sampler;
    // dashboard tiles refresh at this rate. LogicalMeterHandle's
    // try_new internally loops on the graph build until it
    // succeeds, so a topology mid-mutation just delays this call
    // rather than returning Err.
    let config = LogicalMeterConfig::new(chrono::TimeDelta::seconds(1));
    let lm = match LogicalMeterHandle::try_new(client.clone(), config).await {
        Ok(lm) => lm,
        Err(e) => {
            log::error!("microgrid loopback: logical-meter setup failed: {e}");
            return false;
        }
    };
    let mut mg = Microgrid::new_from_handles(client, lm);
    // First cursor reset, BEFORE the new forwarders spawn: a stream that
    // stalled long before this rebuild left an hours-old cursor, and a new
    // forwarder reviving it would otherwise integrate a trapezoid across
    // that whole gap the moment its first sample lands.
    reset_energy_cursors(slot);
    let handles = subscribe_power_forwarders(&mut mg, site, slot.clone()).await;
    // Atomic swap. Aborting the old forwarders + dropping the old
    // Microgrid happens AFTER the new LM has subscribed to every
    // component it cares about (above), so the shared client's
    // per-component broadcast Senders never see receiver_count drop
    // to zero between LM generations.
    for h in slot.forwarders.lock().drain(..) {
        h.abort();
    }
    // The energy bookkeeping runs only now, with the old forwarders gone —
    // earlier, their still-arriving samples would re-seed a just-reset
    // cursor or repopulate a just-cleared map. The new forwarders are
    // already live, but their samples carry the new topology: a cleared
    // total loses at most the sub-second span since their subscribe, and a
    // reset cursor just re-seeds on their next sample.
    //
    // A site reset (config hot-reload) started a new run: the site cleared
    // its per-component energy accumulators, so the aggregate totals clear
    // too. A plain topology mutation keeps them (same generation).
    clear_energy_on_new_run(slot, site);
    // Second cursor reset: any cursor present now was re-seeded during the
    // handoff (by an old forwarder up to the abort, or a new one already
    // delivering). Dropping it costs at most one real ~1 s interval and
    // guarantees no stream integrates across the swap gap.
    reset_energy_cursors(slot);
    slot.latest.write().clear();
    // The sparkline rings too — a rebuild that drops a stream
    // category (no more PV, say) must not keep serving the stale
    // series via /api/mg/{id}/microgrid/history forever.
    slot.history.write().clear();
    // The cumulative energy totals, however, survive the rebuild —
    // re-expose them in the just-cleared cache.
    republish_energy_totals(slot, site);
    *slot.forwarders.lock() = handles;
    *slot.microgrid.write() = Some(mg);
    true
}

/// Subscribe to MicrogridSite events and rebuild the Microgrid handle on
/// every TopologyChanged. Lagged-receiver and dropped-sender
/// events also trigger a rebuild (defensive — a missed event
/// might have been a topology change).
async fn run_supervisor(
    grpc_url: String,
    slot: SharedMicrogrid,
    site: MicrogridSite,
    mut events: tokio::sync::broadcast::Receiver<SiteEvent>,
) {
    loop {
        match events.recv().await {
            Ok(SiteEvent::TopologyChanged { .. }) => {
                debounce_topology_burst(&mut events).await;
                rebuild(&grpc_url, &slot, &site).await;
            }
            Ok(_) => continue,
            Err(RecvError::Lagged(n)) => {
                log::warn!(
                    "microgrid loopback supervisor: lagged {n} events, rebuilding defensively"
                );
                debounce_topology_burst(&mut events).await;
                rebuild(&grpc_url, &slot, &site).await;
            }
            Err(RecvError::Closed) => {
                log::info!("microgrid loopback supervisor: site events closed, exiting");
                return;
            }
        }
    }
}

/// After seeing the first TopologyChanged, swallow any further
/// events that arrive within `DEBOUNCE` so a hot-reload that
/// registers 12 components in rapid succession only triggers one
/// rebuild instead of 12.
async fn debounce_topology_burst(events: &mut tokio::sync::broadcast::Receiver<SiteEvent>) {
    const DEBOUNCE: Duration = Duration::from_millis(300);
    let deadline = tokio::time::Instant::now() + DEBOUNCE;
    loop {
        match tokio::time::timeout_at(deadline, events.recv()).await {
            Ok(Ok(_)) => continue, // keep collecting
            Ok(Err(_)) => return,  // broadcast error; supervisor's main loop deals with it
            Err(_) => return,      // deadline; we're done
        }
    }
}

/// Rebuild the `LogicalMeterHandle` so its graph snapshot reflects
/// the new topology. `build_microgrid` does the work — it
/// subscribes the new forwarders first, then atomically aborts the
/// old ones and swaps the slot. The old `Microgrid` stays in the
/// slot until then so the shared client's per-component broadcast
/// Senders keep at least one live receiver across the handoff.
///
/// Only the `LogicalMeterHandle` inside the new Microgrid is
/// rebuilt; the `MicrogridClientHandle` cached in `slot.client` is
/// reused. See the field doc for why the client is long-lived.
async fn rebuild(grpc_url: &str, slot: &SharedMicrogrid, site: &MicrogridSite) {
    log::info!("microgrid loopback: topology changed — rebuilding handle");
    build_microgrid(grpc_url, slot, site).await;
}

/// Build subscriptions for the active-power streams the Dashboard
/// tier-1 (grid), tier-2 (battery pool), tier-3 (PV), and tier-4
/// (consumer + producer aggregates) read from, and spawn one tokio
/// task per surviving subscription to forward samples onto the
/// MicrogridSite event bus.
///
/// Each `formula.subscribe().await` is run on the caller's task so
/// that, when this function returns, the new LM has already
/// subscribed all its required components through the shared
/// client. That keeps `build_microgrid`'s swap step safe: the old
/// `Microgrid` can drop without ever taking the shared client's
/// per-component broadcast receiver count to zero.
///
/// Streams whose underlying category is absent (no PV in the
/// topology, etc.) emit a single `log::info!` and are silently
/// dropped — the Dashboard's matching tile renders as "data
/// unavailable" until that category appears.
async fn subscribe_power_forwarders(
    microgrid: &mut Microgrid,
    site: &MicrogridSite,
    state: SharedMicrogrid,
) -> Vec<JoinHandle<()>> {
    let mut handles = Vec::new();
    let lm = microgrid.logical_meter();
    let metered: [(&'static str, _); 4] = [
        ("grid_power", lm.grid::<metric::AcPowerActive>()),
        ("consumer_power", lm.consumer::<metric::AcPowerActive>()),
        ("producer_power", lm.producer::<metric::AcPowerActive>()),
        ("pv_power", lm.pv::<metric::AcPowerActive>(None)),
    ];
    for (stream, formula) in metered {
        if let Some(h) = subscribe_power_forwarder(stream, formula, site, state.clone()).await {
            handles.push(h);
        }
    }
    // Only the grid formula — no consumer/producer/pv reactive streams
    // (spec: one site tile).
    if let Some(h) = subscribe_reactive_forwarder(
        "grid_reactive_power",
        lm.grid::<metric::AcPowerReactive>(),
        site,
        state.clone(),
    )
    .await
    {
        handles.push(h);
    }
    // Grid frequency via `lm.grid::<metric::AcFrequency>()` would
    // be the natural way to feed a "Grid frequency" tile, but the
    // LogicalMeterActor's `TypedFormulaResponseSender` branches only
    // on Power / Voltage / ReactivePower / Current — calling
    // `.subscribe()` on the Frequency formula returns `Internal:
    // Can't create TypedFormulaResponseSender for ...Frequency`.
    // Still true as of frequenz-microgrid 0.5.0: it declares the
    // `AcFrequency` metric (a CoalesceFormula) but never wires a
    // Frequency sender arm. Until that lands, frequency stays on the
    // per-component /api/history?metric=frequency_hz path.
    // BatteryPool takes &mut self for power() / power_bounds() (it
    // caches subscriber refs); build it once and let it go out of
    // scope after both subscriptions resolve.
    match microgrid.battery_pool(None) {
        Ok(mut pool) => {
            if let Some(h) =
                subscribe_power_forwarder("battery_pool_power", pool.power(), site, state.clone())
                    .await
            {
                handles.push(h);
            }
            // power_bounds returns a Vec<Bounds<Power>>; the
            // forwarder flattens the first envelope into two
            // separate streams so the existing point-sample
            // infrastructure (cache + sparkline) renders both
            // halves without an envelope-shaped payload variant.
            handles.push(spawn_bounds_forwarder(pool.power_bounds(), site, state));
        }
        Err(e) => log::info!("microgrid loopback: battery pool absent — skipping: {e}"),
    }
    handles
}

/// Forward a `Vec<Bounds<Power>>` stream as two point streams
/// `battery_pool_bounds_lower` + `battery_pool_bounds_upper`. The
/// upstream tracker emits a fresh Vec on every telemetry snapshot,
/// so the cadence matches the power forwarders' 1 Hz; sparklines
/// alongside the pool power tile track the same time axis.
///
/// When the Vec is empty (no batteries in the pool) both halves
/// publish `None`. When it has multiple disjoint regions we keep
/// only the outermost envelope — single-region is by far the
/// common case and a multi-region split is a niche signal that the
/// developer-facing dashboard isn't designed around.
fn spawn_bounds_forwarder(
    mut rx: tokio::sync::broadcast::Receiver<Vec<frequenz_microgrid::Bounds<Power>>>,
    site: &MicrogridSite,
    state: SharedMicrogrid,
) -> JoinHandle<()> {
    let site = site.clone();
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(envelopes) => {
                    let lower = outer_bound(&envelopes, |b| b.lower(), f32::min);
                    let upper = outer_bound(&envelopes, |b| b.upper(), f32::max);
                    let ts_ms = chrono::Utc::now().timestamp_millis();
                    publish_scalar(
                        "battery_pool_bounds_lower",
                        "Power",
                        "W",
                        lower,
                        ts_ms,
                        &site,
                        &state,
                    );
                    publish_scalar(
                        "battery_pool_bounds_upper",
                        "Power",
                        "W",
                        upper,
                        ts_ms,
                        &site,
                        &state,
                    );
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    log::warn!("microgrid loopback: battery_pool_bounds lagged {n} samples");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    log::info!("microgrid loopback: battery_pool_bounds closed; forwarder exiting");
                    return;
                }
            }
        }
    })
}

fn outer_bound(
    envelopes: &[frequenz_microgrid::Bounds<Power>],
    pick: impl Fn(&frequenz_microgrid::Bounds<Power>) -> Option<Power>,
    fold: fn(f32, f32) -> f32,
) -> Option<f32> {
    envelopes
        .iter()
        .filter_map(|b| pick(b).map(|p| p.as_watts()))
        .reduce(fold)
}

/// Subscribe to one Power-valued formula and spawn a forwarder that
/// pushes each `Sample<Power>` onto the MicrogridSite event bus as a
/// `MicrogridSample { stream, quantity: "Power", unit: "W", ... }`
/// event. The `formula.subscribe().await` runs on the caller's task
/// so the LM has actually registered for the component samples by
/// the time we return — see `build_microgrid` for why that ordering
/// matters across rebuilds. Returns `None` (no spawn) if the formula
/// errored at construction (typical for absent categories) or the
/// initial subscribe failed.
async fn subscribe_power_forwarder(
    stream: &'static str,
    formula: Result<frequenz_microgrid::Formula<Power>, frequenz_microgrid::Error>,
    site: &MicrogridSite,
    state: SharedMicrogrid,
) -> Option<JoinHandle<()>> {
    let formula = match formula {
        Ok(f) => f,
        Err(e) => {
            log::info!("microgrid loopback: skip {stream} ({e})");
            return None;
        }
    };
    let mut rx = match formula.subscribe().await {
        Ok(rx) => rx,
        Err(e) => {
            log::warn!("microgrid loopback: subscribe {stream} failed: {e}");
            return None;
        }
    };
    let site = site.clone();
    Some(tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(sample) => publish_power(stream, sample, &site, &state),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    log::warn!("microgrid loopback: {stream} lagged {n} samples");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    log::info!("microgrid loopback: {stream} closed; forwarder exiting");
                    return;
                }
            }
        }
    }))
}

fn publish_power(
    stream: &'static str,
    sample: Sample<Power>,
    site: &MicrogridSite,
    state: &SharedMicrogrid,
) {
    let value = sample.value().map(|p| p.as_watts());
    let ts_ms = sample.timestamp().timestamp_millis();
    // Integrate this aggregate into a companion cumulative-energy stream
    // (`grid_power` → `grid_energy`, …) before the new power overwrites the
    // cached sample the integral reads from.
    if let Some(energy_stream) = energy_stream_for(stream) {
        accumulate_energy(energy_stream, value, ts_ms, site, state);
    }
    publish_scalar(stream, "Power", "W", value, ts_ms, site, state);
}

/// Subscribe to one ReactivePower-valued formula and spawn a forwarder that
/// pushes each `Sample<ReactivePower>` onto the MicrogridSite event bus as a
/// `MicrogridSample { stream, quantity: "ReactivePower", unit: "var", ... }`
/// event. Kept as a parallel pair with `subscribe_power_forwarder` /
/// `publish_power` rather than a generic-over-quantity helper: that keeps
/// the (far more heavily used) Power path monomorphic and easy to read,
/// at the cost of a little duplication for this one Q stream. The
/// `formula.subscribe().await` runs on the caller's task so the LM has
/// actually registered for the component samples by the time we return —
/// see `build_microgrid` for why that ordering matters across rebuilds.
/// Returns `None` (no spawn) if the formula errored at construction
/// (typical for absent categories) or the initial subscribe failed.
async fn subscribe_reactive_forwarder(
    stream: &'static str,
    formula: Result<frequenz_microgrid::Formula<ReactivePower>, frequenz_microgrid::Error>,
    site: &MicrogridSite,
    state: SharedMicrogrid,
) -> Option<JoinHandle<()>> {
    let formula = match formula {
        Ok(f) => f,
        Err(e) => {
            log::info!("microgrid loopback: skip {stream} ({e})");
            return None;
        }
    };
    let mut rx = match formula.subscribe().await {
        Ok(rx) => rx,
        Err(e) => {
            log::warn!("microgrid loopback: subscribe {stream} failed: {e}");
            return None;
        }
    };
    let site = site.clone();
    Some(tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(sample) => publish_reactive(stream, sample, &site, &state),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    log::warn!("microgrid loopback: {stream} lagged {n} samples");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    log::info!("microgrid loopback: {stream} closed; forwarder exiting");
                    return;
                }
            }
        }
    }))
}

fn publish_reactive(
    stream: &'static str,
    sample: Sample<ReactivePower>,
    site: &MicrogridSite,
    state: &SharedMicrogrid,
) {
    let value = sample.value().map(|q| q.as_volt_amperes_reactive());
    let ts_ms = sample.timestamp().timestamp_millis();
    // No energy hook on purpose: reactive energy (varh) accumulation is
    // out of scope; energy_stream_for never maps this stream.
    publish_scalar(stream, "ReactivePower", "var", value, ts_ms, site, state);
}

/// The cumulative-energy companion stream for a metered power stream, or
/// `None` for streams we don't integrate (e.g. the bounds envelopes).
fn energy_stream_for(power_stream: &str) -> Option<&'static str> {
    Some(match power_stream {
        "grid_power" => "grid_energy",
        "consumer_power" => "consumer_energy",
        "producer_power" => "producer_energy",
        "pv_power" => "pv_energy",
        "battery_pool_power" => "battery_pool_energy",
        _ => return None,
    })
}

/// Clear the aggregate energy totals when `site`'s run generation moved
/// past the one the slot's totals were gathered under: the site was reset
/// (a config hot-reload), so the energy belongs to a previous run — just
/// like the site's own per-component accumulators, which `reset()` clears.
/// Same-generation rebuilds (topology mutations) keep the totals.
fn clear_energy_on_new_run(state: &SharedMicrogrid, site: &MicrogridSite) {
    let generation = site.run_generation();
    if state
        .energy_generation
        .swap(generation, std::sync::atomic::Ordering::Relaxed)
        != generation
    {
        state.energy.write().clear();
    }
}

/// Drop every aggregate integrator cursor (`EnergyAccum::reset_cursor`),
/// so the first power sample each stream sees afterwards re-seeds instead
/// of integrating a trapezoid across a dead window using a stale cursor.
/// A rebuild runs it twice: before the new forwarders spawn (a stream
/// that stalled long ago left an old cursor a revived stream would
/// integrate across) and again after the old forwarders are aborted (a
/// still-live forwarder re-seeds cursors during the handoff).
fn reset_energy_cursors(state: &SharedMicrogrid) {
    for e in state.energy.write().values_mut() {
        e.reset_cursor();
    }
}

/// Re-expose the retained energy totals after a rebuild cleared
/// `latest`/`history`, so a read of e.g. `pv_energy` still returns the
/// accumulated value even when that stream's power never fires again (a
/// topology that dropped PV) and would otherwise never repopulate the
/// cleared cache. Streams a new forwarder already repopulated are left
/// alone — their cached sample is newer than this wall-clock-stamped
/// republish. (A racing sample between the check and the publish can
/// still be overwritten, but only on a live stream, whose next 1 Hz
/// sample corrects it.)
///
/// A no-op on first boot, when `state.energy` is still empty.
fn republish_energy_totals(state: &SharedMicrogrid, site: &MicrogridSite) {
    let ts_ms = chrono::Utc::now().timestamp_millis();
    // Snapshot (stream, total) under the energy lock, then publish outside
    // it — publish_scalar takes the latest/history locks.
    let totals: Vec<(&'static str, f64)> = state
        .energy
        .read()
        .iter()
        .map(|(stream, e)| (*stream, e.total_wh))
        .collect();
    for (stream, total) in totals {
        if state.latest.read().contains_key(stream) {
            continue;
        }
        publish_scalar(
            stream,
            "Energy",
            "Wh",
            Some(total as f32),
            ts_ms,
            site,
            state,
        );
    }
}

/// Advance an aggregate energy total by the trapezoid between the last power
/// sample and the incoming one, and publish the running total. The total
/// lives in `state.energy` (persistent across rebuilds), not in `latest`
/// (cleared on rebuild) — so a topology mutation mid-run doesn't reset the
/// integral. A `None` power (stream gap) carries the total forward without
/// advancing it. Signed like the power, so the total is net energy (Wh).
fn accumulate_energy(
    energy_stream: &'static str,
    new_value: Option<f32>,
    new_ts_ms: i64,
    site: &MicrogridSite,
    state: &SharedMicrogrid,
) {
    let total = {
        let mut acc = state.energy.write();
        let e = acc.entry(energy_stream).or_default();
        // A None power (stream gap) carries the total forward without
        // advancing it; a real sample integrates the trapezoid since the
        // previous one.
        if let Some(nv) = new_value {
            e.advance(nv, new_ts_ms);
        }
        e.total_wh
    };
    publish_scalar(
        energy_stream,
        "Energy",
        "Wh",
        Some(total as f32),
        new_ts_ms,
        site,
        state,
    );
}

/// Push a typed scalar onto both the per-stream `latest` cache and
/// the WS event bus. The `quantity` + `unit` pair travels with the
/// sample so the SPA picks the right autoscale family (Power
/// W→kW→MW, Frequency Hz, etc.) without pattern-matching on the
/// stream name.
fn publish_scalar(
    stream: &'static str,
    quantity: &'static str,
    unit: &'static str,
    value: Option<f32>,
    ts_ms: i64,
    site: &MicrogridSite,
    state: &SharedMicrogrid,
) {
    let snapshot = MicrogridSampleSnapshot {
        quantity,
        unit,
        ts_ms,
        value,
    };
    state.latest.write().insert(stream, snapshot);
    // Append to the rolling history ring so the Dashboard tile
    // sparklines have past data to backfill from on page load.
    // Drop the oldest entry when the ring is full.
    {
        let mut history = state.history.write();
        let ring = history.entry(stream).or_default();
        if ring.len() == MICROGRID_HISTORY_CAP {
            ring.pop_front();
        }
        ring.push_back(HistorySample { ts_ms, value });
    }
    site.broadcast_microgrid_sample(stream, quantity, unit, ts_ms, value);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::state::new_microgrid_slot;

    const HOUR_MS: i64 = 3_600_000;

    #[test]
    fn energy_stream_for_maps_aggregates_and_ignores_the_rest() {
        assert_eq!(energy_stream_for("grid_power"), Some("grid_energy"));
        assert_eq!(energy_stream_for("pv_power"), Some("pv_energy"));
        assert_eq!(
            energy_stream_for("battery_pool_power"),
            Some("battery_pool_energy")
        );
        // Bounds envelopes and unknown streams don't integrate.
        assert_eq!(energy_stream_for("active_power_lower_bound_w"), None);
    }

    #[test]
    fn rebuild_keeps_energy_total_and_reseeds_after_the_gap() {
        let state = new_microgrid_slot();
        let site = MicrogridSite::new();
        // Move 1000 Wh on grid_energy: 1000 W held for one hour.
        accumulate_energy("grid_energy", Some(1000.0), 0, &site, &state);
        accumulate_energy("grid_energy", Some(1000.0), HOUR_MS, &site, &state);
        let before = state.energy.read()["grid_energy"].total_wh;
        assert!((before - 1000.0).abs() < 1e-6, "{before}");

        // A rebuild resets the cursors (before its forwarders spawn),
        // clears `latest`, then republishes the retained totals.
        reset_energy_cursors(&state);
        state.latest.write().clear();
        republish_energy_totals(&state, &site);

        // #2: the retained total is republished into the cleared `latest`,
        // so a read still sees it even though no power sample has fired
        // since the rebuild (e.g. a topology that dropped the stream).
        let republished = state.latest.read()["grid_energy"].value.unwrap();
        assert!((republished as f64 - before).abs() < 1.0, "{republished}");

        // #1: the first sample after the rebuild re-seeds instead of
        // integrating a trapezoid across the (here 10 h) dead window — the
        // total is unchanged until a real interval elapses.
        accumulate_energy(
            "grid_energy",
            Some(1000.0),
            HOUR_MS + 10 * HOUR_MS,
            &site,
            &state,
        );
        let after_seed = state.energy.read()["grid_energy"].total_wh;
        assert!(
            (after_seed - before).abs() < 1e-6,
            "gap bridged: {after_seed}"
        );

        // Integration then resumes cleanly from the new cursor (+1 h).
        accumulate_energy(
            "grid_energy",
            Some(1000.0),
            HOUR_MS + 10 * HOUR_MS + HOUR_MS,
            &site,
            &state,
        );
        let resumed = state.energy.read()["grid_energy"].total_wh;
        assert!((resumed - (before + 1000.0)).abs() < 1e-6, "{resumed}");
    }

    /// A forwarder sample that lands between the `latest` clear and the
    /// republish owns the fresher value — the republish must not
    /// overwrite it with the wall-clock-stamped copy.
    #[test]
    fn republish_skips_streams_a_forwarder_already_repopulated() {
        let state = new_microgrid_slot();
        let site = MicrogridSite::new();
        accumulate_energy("grid_energy", Some(1000.0), 0, &site, &state);
        reset_energy_cursors(&state);
        state.latest.write().clear();
        // A new forwarder fires before the republish runs.
        accumulate_energy("grid_energy", Some(1000.0), HOUR_MS, &site, &state);
        let fresh_ts = state.latest.read()["grid_energy"].ts_ms;
        republish_energy_totals(&state, &site);
        assert_eq!(state.latest.read()["grid_energy"].ts_ms, fresh_ts);
    }

    /// A config reload resets the site and bumps its run generation: the
    /// aggregate totals belong to the previous run and clear, matching
    /// the per-component accumulators `reset()` cleared. A rebuild in the
    /// same generation (a topology mutation) keeps them.
    #[test]
    fn energy_clears_on_a_new_run_generation_only() {
        let state = new_microgrid_slot();
        let site = MicrogridSite::new();
        accumulate_energy("grid_energy", Some(1000.0), 0, &site, &state);
        accumulate_energy("grid_energy", Some(1000.0), HOUR_MS, &site, &state);

        // Same generation: a mutation rebuild keeps the totals.
        clear_energy_on_new_run(&state, &site);
        assert!(!state.energy.read().is_empty());

        // A reset (config reload) starts a new run: the totals clear.
        site.reset();
        clear_energy_on_new_run(&state, &site);
        assert!(state.energy.read().is_empty());
    }

    #[test]
    fn grid_reactive_power_has_no_energy_stream() {
        assert_eq!(energy_stream_for("grid_reactive_power"), None);
    }
}
