//! Per-component runtime knobs that simulate device-side faults
//! independent of the physics layer.
//!
//! These three orthogonal flags let a spec (or a runtime caller via
//! `POST /api/mg/{mg}/component/{id}/status`) drive faulty behaviour
//! without touching the simulated state. They live in the
//! MicrogridSite, not on the component, because the things they
//! control (gRPC stream pacing, request handling) are server-facing
//! concerns the components shouldn't know about.

#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    Default,
    serde::Serialize,
    serde::Deserialize,
    schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Health {
    /// Component reports the physics-derived state code (ready /
    /// charging / discharging / …) and accepts setpoints normally.
    #[default]
    Ok,
    /// Component reports `ERROR`; setpoint requests are rejected at
    /// the gRPC layer with `FailedPrecondition`.
    Error,
    /// Component reports `STANDBY`; setpoint requests are rejected.
    Standby,
}

#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    Default,
    serde::Serialize,
    serde::Deserialize,
    schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum TelemetryMode {
    /// Emit samples at the component's stream interval (the default).
    #[default]
    Normal,
    /// Connection stays open but no samples are sent. Models a device
    /// that's reachable on the wire but has lost its data link to the
    /// metering / telemetry pipeline.
    Silent,
    /// Stream task exits as soon as it sees this. Existing clients
    /// see EOF; new connections terminate immediately. Models an
    /// unreachable device.
    Closed,
    /// Emit samples at the stream interval but with empty
    /// `metric_samples` and a single `ERROR` state snapshot. Models a
    /// device whose stream is alive but carries only an error state
    /// and no parseable metrics, so downstream consumers receive no
    /// data.
    ErrorEmpty,
    /// The component exists in the graph (so clients discover and
    /// subscribe to it), but every telemetry-stream request is
    /// rejected with gRPC `NOT_FOUND`. Models a phantom component:
    /// present enough to be discovered and subscribed to, but with no
    /// data channel behind it, so a streaming client keeps retrying
    /// the subscription indefinitely.
    NotFound,
}

#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    Default,
    serde::Serialize,
    serde::Deserialize,
    schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum CommandMode {
    /// Validate against bounds and apply.
    #[default]
    Normal,
    /// Hang the request indefinitely. The client will time out
    /// according to its own deadline. Models a device whose control
    /// channel is alive but stuck.
    Timeout,
    /// Reply immediately with `Unavailable`. Models a device whose
    /// control channel is down.
    Error,
    /// Reject non-zero active-power setpoints with gRPC
    /// `INVALID_ARGUMENT`, even though the advertised bounds said the
    /// setpoint was within range. Models a device that advertises one
    /// set of bounds but then rejects a command of that size against
    /// a tighter internal limit. Faulting is intermittent and rotates
    /// per component (see `over_bound_faulty_now`). Zero-power
    /// (fail-safe) setpoints are still accepted.
    OverBound,
}

display_as_serde!(Health, TelemetryMode, CommandMode);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ComponentRuntime {
    pub health: Health,
    pub telemetry: TelemetryMode,
    pub command: CommandMode,
}

impl Health {
    /// Proto state-code label corresponding to this health state.
    /// Returned to clients as part of every telemetry sample's
    /// state_snapshot when health is non-Ok; physics-derived labels
    /// take over when Ok.
    pub fn state_label(self) -> Option<&'static str> {
        match self {
            Self::Ok => None,
            Self::Error => Some("error"),
            Self::Standby => Some("standby"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CommandMode, Health, TelemetryMode};

    #[test]
    fn display_uses_the_snake_case_name() {
        assert_eq!(TelemetryMode::ErrorEmpty.to_string(), "error_empty");
        assert_eq!(CommandMode::OverBound.to_string(), "over_bound");
    }

    #[test]
    fn the_modes_read_and_write_their_snake_case_names() {
        let mode: TelemetryMode = serde_json::from_str(r#""error_empty""#).unwrap();
        assert_eq!(mode, TelemetryMode::ErrorEmpty);
        assert_eq!(
            serde_json::to_string(&CommandMode::OverBound).unwrap(),
            r#""over_bound""#
        );
        assert!(serde_json::from_str::<Health>(r#""borked""#).is_err());
    }
}
