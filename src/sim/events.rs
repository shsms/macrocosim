//! MicrogridSite-level event stream — fans out telemetry samples and topology
//! version bumps to UI subscribers (the WebSocket endpoint, future
//! command-line monitors, anything else that wants live notifications).
//!
//! `tokio::sync::broadcast` is multi-producer / multi-consumer with a
//! bounded ring; if a subscriber falls behind by more than the ring
//! capacity it gets a `RecvError::Lagged` and skips ahead. That's the
//! right tradeoff for a UI: the chart is stale anyway when the tab
//! has been backgrounded for an hour, no value in queueing every
//! missed sample.

use serde::Serialize;

/// Capacity of the broadcast ring. Sized to absorb a full second of
/// telemetry samples (~10 metrics × ~50 components) plus headroom for
/// write bursts. Subscribers that fall further behind than this will
/// see `Lagged` errors and re-sync.
pub const EVENT_BUS_CAPACITY: usize = 4096;

/// A single broadcast event the UI can react to. The discriminator
/// is `kind`; per-variant fields are inlined alongside it (serde
/// `tag` + `flatten`-equivalent via per-variant struct shape).
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SiteEvent {
    /// Something the topology payload shows changed (a rebuild, a
    /// status or mode write, …). Subscribers should refetch
    /// /api/mg/{mg}/topology if they care about structure or metadata
    /// changes. A cheap signal.
    TopologyChanged { version: u64 },
    /// Single telemetry sample, emitted by the history sampler.
    Sample {
        #[serde(rename = "component_id")]
        id: u64,
        metric: &'static str,
        #[serde(
            rename = "ts",
            serialize_with = "crate::timefmt::serialize_millis_as_rfc3339"
        )]
        ts_ms: i64,
        /// Unit of `value`, from the metric.
        unit: &'static str,
        value: f32,
    },
    /// Control-app setpoint event — fires for every gRPC SetActive /
    /// SetReactive / AugmentBounds the server processed (regardless
    /// of accept / reject). UI inspector appends to the live list.
    /// Field is `setpoint_kind` (not `kind`) to avoid colliding with
    /// the parent enum's serde `tag = "kind"` discriminator.
    Setpoint {
        #[serde(rename = "component_id")]
        id: u64,
        #[serde(
            rename = "ts",
            serialize_with = "crate::timefmt::serialize_millis_as_rfc3339"
        )]
        ts_ms: i64,
        /// Lowercase token: "active_power" / "reactive_power" /
        /// "augment_bounds" (active axis) / "augment_reactive_bounds".
        setpoint_kind: &'static str,
        /// Unit of `value`, from the setpoint kind.
        unit: &'static str,
        value: f32,
        accepted: bool,
        /// Only set when `accepted == false` — the gRPC error message
        /// the client received.
        reason: Option<String>,
    },
    /// One captured log record. Fanned out from `ui_log::LogTap` via
    /// the WS handler — the handler subscribes to LOG_TAP separately
    /// and re-emits as this variant so the SPA's single WS stream
    /// covers everything.
    Log {
        #[serde(
            rename = "ts",
            serialize_with = "crate::timefmt::serialize_millis_as_rfc3339"
        )]
        ts_ms: i64,
        level: String,
        target: String,
        message: String,
    },
    /// One sample from an aggregated metric stream that the loopback
    /// Microgrid client exposes — grid_power, battery_pool_power,
    /// pv_power, consumer_power, producer_power, etc. (see
    /// `ui::spawn_microgrid_loopback` for the set of streams).
    /// `value` is the f32 magnitude in the base `unit`; `None` means
    /// the formula has no current value (e.g. the source category
    /// has no live samples in the configured `LogicalMeterConfig`
    /// resampling window). The SPA's Dashboard tiles pick by
    /// `stream` and apply auto-scale on `unit`.
    MicrogridSample {
        stream: &'static str,
        /// Quantity type name — `"Power"` / `"Voltage"` / `"Frequency"` /
        /// `"Percentage"` / etc. Matches `frequenz_microgrid::quantity`'s
        /// type names. Lets the SPA group same-quantity tiles onto a
        /// shared visual baseline without parsing the unit string.
        quantity: &'static str,
        /// Base unit string — `"W"` / `"VAr"` / `"V"` / `"Hz"` / `"%"`.
        unit: &'static str,
        #[serde(
            rename = "ts",
            serialize_with = "crate::timefmt::serialize_millis_as_rfc3339"
        )]
        ts_ms: i64,
        value: Option<f32>,
    },
    /// A dispatch was created / updated / deleted in the enterprise
    /// dispatch store. The SPA's per-microgrid Dispatches view
    /// refetches `/api/mg/{id}/dispatches` when the carried `microgrid_id`
    /// (set on the `WireEvent` wrapper) matches the microgrid it's
    /// showing. Emitted directly by the WS event pump from its
    /// `DispatchStore` subscription — like the `Log` variant — rather
    /// than from a per-site event bus, since the dispatch store is
    /// enterprise-wide and lives on `Config`, not on a `MicrogridSite`.
    DispatchChanged {
        dispatch_id: u64,
        /// `"created"` / `"updated"` / `"deleted"`.
        change: &'static str,
    },
    /// The microgrid named by the `WireEvent` wrapper's
    /// `microgrid_id` was unloaded. Emitted by the WS event pump from
    /// the engine's `microgrid_unloaded` broadcast; the SPA drops its
    /// card and leaves a view of it.
    MicrogridUnloaded,
    /// A runtime knob changed through the typed control API, from a
    /// script, a terminal or the web UI; the control handlers emit
    /// this. The inspector refreshes its edit-in-place inputs from
    /// it.
    KnobChanged {
        /// `None` for a knob of the whole microgrid (`grid-voltage`,
        /// `sample-lag`).
        #[serde(rename = "component_id")]
        id: Option<u64>,
        #[serde(
            rename = "ts",
            serialize_with = "crate::timefmt::serialize_millis_as_rfc3339"
        )]
        ts_ms: i64,
        /// One of: "meter-power" / "meter-reactive-power" /
        /// "meter-power-factor" / "solar-sunlight" / "boiler-demand" /
        /// "boiler-pressure" / "ev" / "reactive-pf-limit" /
        /// "reactive-apparent-va" / "operational-mode" /
        /// "grid-voltage" / "sample-lag".
        knob: &'static str,
        /// Unit of `value`, from the knob; `None` for a unitless
        /// ratio or a token.
        unit: Option<&'static str>,
        /// New value; None when the knob was cleared: a meter's
        /// power, reactive power or power factor, an unplugged EV, or
        /// a removed pf-limit / apparent-va.
        value: Option<KnobValue>,
        /// meter-power-factor only.
        leading: Option<bool>,
    },
}

/// The value of a `KnobChanged`, serialized as the bare JSON value.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(untagged)]
pub enum KnobValue {
    Number(f32),
    /// An operational mode, by its snake_case name.
    Mode(crate::sim::OperationalMode),
    /// One number per phase, such as the grid voltage.
    List(Vec<f32>),
}

/// Unit of a `KnobChanged` value, by knob token. `None` for the
/// unitless power-factor knobs and the operational mode, and for a
/// token with no entry here (a debug build panics on one, so a new
/// knob gets its unit).
pub fn knob_unit(knob: &str) -> Option<&'static str> {
    match knob {
        "meter-power" => Some("W"),
        "meter-reactive-power" => Some("VAr"),
        "solar-sunlight" | "ev" => Some("%"),
        "boiler-demand" => Some("kg/s"),
        "boiler-pressure" => Some("bar"),
        "reactive-apparent-va" => Some("VA"),
        "grid-voltage" => Some("V"),
        "sample-lag" => Some("s"),
        "meter-power-factor" | "reactive-pf-limit" | "operational-mode" => None,
        other => {
            debug_assert!(false, "knob_unit: no entry for knob {other}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json(ev: &SiteEvent) -> serde_json::Value {
        serde_json::to_value(ev).unwrap()
    }

    #[test]
    fn events_name_the_component_and_the_time_in_full() {
        let ts_ms = 1_791_288_000_000;
        let sample = json(&SiteEvent::Sample {
            id: 7,
            metric: "reactive_power_var",
            ts_ms,
            unit: "VAr",
            value: 1.0,
        });
        assert_eq!(sample["component_id"], 7);
        assert_eq!(sample["ts"], "2026-10-06T12:00:00.000Z");
        assert_eq!(sample["unit"], "VAr");
        let setpoint = json(&SiteEvent::Setpoint {
            id: 7,
            ts_ms,
            setpoint_kind: "reactive_power",
            unit: "VAr",
            value: 1.0,
            accepted: true,
            reason: None,
        });
        assert_eq!(setpoint["component_id"], 7);
        assert_eq!(setpoint["unit"], "VAr");
        let knob = json(&SiteEvent::KnobChanged {
            id: Some(7),
            ts_ms,
            knob: "reactive-apparent-va",
            unit: knob_unit("reactive-apparent-va"),
            value: Some(KnobValue::Number(1.5)),
            leading: None,
        });
        assert_eq!(knob["component_id"], 7);
        assert_eq!(knob["unit"], "VA");
        assert_eq!(knob["value"], 1.5);
        for ev in [sample, setpoint, knob] {
            assert!(ev.get("id").is_none() && ev.get("ts_ms").is_none(), "{ev}");
        }
        let log = json(&SiteEvent::Log {
            ts_ms,
            level: "info".into(),
            target: "t".into(),
            message: "m".into(),
        });
        assert_eq!(log["ts"], "2026-10-06T12:00:00.000Z");
        assert_eq!(
            json(&SiteEvent::MicrogridUnloaded),
            serde_json::json!({"kind": "microgrid_unloaded"})
        );
    }
}
