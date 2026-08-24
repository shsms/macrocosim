//! gRPC integration tests. Each test spawns a fresh `TestServer`,
//! connects via the generated `MicrogridClient`, and exercises the
//! RPC surface end-to-end.

mod common;

use common::TestServer;
use switchyard::proto::common::metrics::{Bounds, Metric};
use switchyard::proto::microgrid::microgrid_client::MicrogridClient;
use switchyard::proto::microgrid::{
    AugmentElectricalComponentBoundsRequest, ListElectricalComponentConnectionsRequest,
    ListElectricalComponentsRequest, PowerType, ReceiveElectricalComponentTelemetryStreamRequest,
    ReceiveElectricalComponentTelemetryStreamResponse, SetElectricalComponentPowerRequest,
    SetElectricalComponentPowerRequestStatus,
};

/// Pull the AC active-power value (W) out of a telemetry response, if present.
fn active_power_w(resp: &ReceiveElectricalComponentTelemetryStreamResponse) -> Option<f32> {
    use switchyard::proto::common::metrics::{Metric, metric_value_variant::MetricValueVariant};
    let t = resp.telemetry.as_ref()?;
    t.metric_samples.iter().find_map(|s| {
        if s.metric != Metric::AcPowerActive as i32 {
            return None;
        }
        match s.value.as_ref()?.metric_value_variant.as_ref()? {
            MetricValueVariant::SimpleMetric(v) => Some(v.value),
            _ => None,
        }
    })
}

/// Pull the `AC_POWER_REACTIVE` sample's bounds out of a telemetry
/// response. `None` when the component published no reactive sample.
fn reactive_sample_bounds(
    resp: &ReceiveElectricalComponentTelemetryStreamResponse,
) -> Option<Vec<Bounds>> {
    let t = resp.telemetry.as_ref()?;
    t.metric_samples
        .iter()
        .find(|s| s.metric == Metric::AcPowerReactive as i32)
        .map(|s| s.bounds.clone())
}

/// Subscribe to `id` and return the first reactive sample that
/// carries bounds. Panics if none arrives within 5 s.
async fn first_reactive_bounds(
    c: &mut MicrogridClient<tonic::transport::Channel>,
    id: u64,
) -> Vec<Bounds> {
    let mut stream = c
        .receive_electrical_component_telemetry_stream(
            ReceiveElectricalComponentTelemetryStreamRequest {
                electrical_component_id: id,
                filter: None,
            },
        )
        .await
        .expect("subscribe")
        .into_inner();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while let Ok(Some(msg)) = stream.message().await {
            match reactive_sample_bounds(&msg) {
                Some(b) if !b.is_empty() => return Some(b),
                _ => continue,
            }
        }
        None
    })
    .await
    .expect("telemetry stream timed out")
    .expect("a reactive sample carrying bounds")
}

/// Subscribe to `id` and block until its AC active power reaches
/// `at_least` W. Panics if it doesn't within 5 s. Lets a test wait for
/// a commanded setpoint to reach the physics loop's published value
/// instead of guessing at a sleep.
async fn wait_for_active_power(
    c: &mut MicrogridClient<tonic::transport::Channel>,
    id: u64,
    at_least: f32,
) {
    let mut stream = c
        .receive_electrical_component_telemetry_stream(
            ReceiveElectricalComponentTelemetryStreamRequest {
                electrical_component_id: id,
                filter: None,
            },
        )
        .await
        .expect("subscribe")
        .into_inner();
    let reached = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while let Ok(Some(msg)) = stream.message().await {
            if active_power_w(&msg).is_some_and(|p| p >= at_least) {
                return true;
            }
        }
        false
    })
    .await
    .expect("telemetry stream timed out");
    assert!(reached, "component {id} never reached {at_least} W");
}

const TINY_TOPOLOGY: &str = r#"
(%make-grid-connection-point :id 1
            :successors
            (list (%make-meter :id 2
                               :successors
                               (list (%make-battery-inverter
                                      :id 4
                                      :rated-lower -5000.0
                                      :rated-upper  5000.0
                                      :successors
                                      (list (%make-battery
                                             :id 3
                                             :rated-lower -5000.0
                                             :rated-upper  5000.0)))))))
"#;

async fn connect(s: &TestServer) -> MicrogridClient<tonic::transport::Channel> {
    MicrogridClient::connect(s.grpc_url.clone())
        .await
        .expect("grpc connect")
}

#[tokio::test(flavor = "multi_thread")]
async fn list_components_returns_topology() {
    let s = TestServer::start(TINY_TOPOLOGY).await;
    let mut c = connect(&s).await;
    let resp = c
        .list_electrical_components(ListElectricalComponentsRequest::default())
        .await
        .expect("list ok")
        .into_inner();
    let ids: Vec<u64> = resp.electrical_components.iter().map(|c| c.id).collect();
    assert!(ids.contains(&1));
    assert!(ids.contains(&2));
    assert!(ids.contains(&3));
    assert!(ids.contains(&4));
}

#[tokio::test(flavor = "multi_thread")]
async fn list_connections_returns_edges() {
    let s = TestServer::start(TINY_TOPOLOGY).await;
    let mut c = connect(&s).await;
    let resp = c
        .list_electrical_component_connections(ListElectricalComponentConnectionsRequest::default())
        .await
        .expect("list ok")
        .into_inner();
    // grid → meter, meter → inverter, inverter → battery.
    assert_eq!(resp.electrical_component_connections.len(), 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn set_power_happy_path_returns_success() {
    let s = TestServer::start(TINY_TOPOLOGY).await;
    let mut c = connect(&s).await;
    let resp = c
        .set_electrical_component_power(SetElectricalComponentPowerRequest {
            electrical_component_id: 4, // the battery inverter
            power: 1000.0,
            power_type: PowerType::Active as i32,
            request_lifetime: Some(30),
        })
        .await
        .expect("set-power ok");
    let mut stream = resp.into_inner();
    let first = stream
        .message()
        .await
        .expect("stream poll")
        .expect("at least one status");
    assert_eq!(
        first.status,
        SetElectricalComponentPowerRequestStatus::Success as i32,
    );
}

const ERRORED_INVERTER_TOPOLOGY: &str = r#"
(%make-grid-connection-point :id 1
            :successors
            (list (%make-meter :id 2
                               :successors
                               (list (%make-battery-inverter
                                      :id 4 :health 'error
                                      :rated-lower -5000.0
                                      :rated-upper  5000.0
                                      :successors
                                      (list (%make-battery
                                             :id 3
                                             :rated-lower -5000.0
                                             :rated-upper  5000.0)))))))
"#;

/// Inverter rated ±5 kW but its battery only ±1 kW, so the combined
/// envelope the gateway must enforce is ±1 kW — narrower than the
/// inverter's own bounds.
const NARROW_BATTERY_TOPOLOGY: &str = r#"
(%make-grid-connection-point :id 1
            :successors
            (list (%make-meter :id 2
                               :successors
                               (list (%make-battery-inverter
                                      :id 4
                                      :rated-lower -5000.0
                                      :rated-upper  5000.0
                                      :successors
                                      (list (%make-battery
                                             :id 3
                                             :rated-lower -1000.0
                                             :rated-upper  1000.0)))))))
"#;

/// An errored inverter refuses every command — both setpoints and bounds
/// augmentations (the latter were previously accepted unconditionally).
#[tokio::test(flavor = "multi_thread")]
async fn errored_component_rejects_power_and_bounds() {
    let s = TestServer::start(ERRORED_INVERTER_TOPOLOGY).await;
    let mut c = connect(&s).await;
    let power_err = c
        .set_electrical_component_power(SetElectricalComponentPowerRequest {
            electrical_component_id: 4,
            power: 0.0,
            power_type: PowerType::Active as i32,
            request_lifetime: Some(30),
        })
        .await
        .expect_err("setpoint to an errored device should be rejected");
    // Erroring the device couples its command mode to Error → Unavailable.
    assert_eq!(power_err.code(), tonic::Code::Unavailable);

    let bounds_err = c
        .augment_electrical_component_bounds(AugmentElectricalComponentBoundsRequest {
            electrical_component_id: 4,
            target_metric: Metric::AcPowerActive as i32,
            bounds: vec![Bounds {
                lower: Some(-1000.0),
                upper: Some(1000.0),
            }],
            request_lifetime: Some(30),
        })
        .await
        .expect_err("bounds augmentation to an errored device should be rejected");
    assert_eq!(bounds_err.code(), tonic::Code::Unavailable);
}

/// An out-of-range `request_lifetime` is a protocol error that must be
/// rejected *before* the setpoint is applied — otherwise the component
/// runs at the commanded power with no expiry timer while the client
/// sees an error.
#[tokio::test(flavor = "multi_thread")]
async fn out_of_range_lifetime_rejects_without_actuating() {
    let s = TestServer::start(TINY_TOPOLOGY).await;
    let mut c = connect(&s).await;
    let err = c
        .set_electrical_component_power(SetElectricalComponentPowerRequest {
            electrical_component_id: 4,
            power: 3000.0,
            power_type: PowerType::Active as i32,
            request_lifetime: Some(5), // below the 10 s minimum
        })
        .await
        .expect_err("sub-minimum lifetime should be rejected");
    assert_eq!(err.code(), tonic::Code::InvalidArgument);

    // The inverter must not have actuated: stream it and confirm it stays
    // at 0 W rather than ramping to the rejected 3 kW (its ramp is instant).
    let resp = c
        .receive_electrical_component_telemetry_stream(
            ReceiveElectricalComponentTelemetryStreamRequest {
                electrical_component_id: 4,
                filter: None,
            },
        )
        .await
        .expect("subscribe");
    let mut stream = resp.into_inner();
    let checked = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let mut seen = 0;
        while let Ok(Some(msg)) = stream.message().await {
            if let Some(p) = active_power_w(&msg) {
                assert!(
                    p.abs() < 1.0,
                    "inverter actuated despite a rejected request: {p} W"
                );
                seen += 1;
                if seen >= 3 {
                    break;
                }
            }
        }
        seen
    })
    .await
    .expect("telemetry stream timed out");
    assert!(checked >= 3, "expected ≥3 power samples, got {checked}");
}

#[tokio::test(flavor = "multi_thread")]
async fn set_power_outside_envelope_is_rejected() {
    let s = TestServer::start(TINY_TOPOLOGY).await;
    let mut c = connect(&s).await;
    // Inverter rated bounds are ±5 kW; +10 kW is outside.
    let err = c
        .set_electrical_component_power(SetElectricalComponentPowerRequest {
            electrical_component_id: 4,
            power: 10_000.0,
            power_type: PowerType::Active as i32,
            request_lifetime: Some(30),
        })
        .await
        .expect_err("expected rejection");
    assert_eq!(err.code(), tonic::Code::FailedPrecondition);
    assert!(
        err.message().contains("envelope") || err.message().contains("bounds"),
        "expected envelope/bounds in message, got {:?}",
        err.message()
    );
}

/// A setpoint inside the inverter's own bounds but outside its battery's
/// (narrower) bounds is rejected against the *intersection* — not
/// silently saturated. The complement of the inverter-only-bounds test.
#[tokio::test(flavor = "multi_thread")]
async fn set_power_outside_battery_inverter_intersection_is_rejected() {
    let s = TestServer::start(NARROW_BATTERY_TOPOLOGY).await;
    let mut c = connect(&s).await;
    // +3 kW: within the inverter's ±5 kW, outside the battery's ±1 kW.
    let err = c
        .set_electrical_component_power(SetElectricalComponentPowerRequest {
            electrical_component_id: 4,
            power: 3_000.0,
            power_type: PowerType::Active as i32,
            request_lifetime: Some(30),
        })
        .await
        .expect_err("expected rejection against the ±1 kW intersection");
    assert_eq!(err.code(), tonic::Code::FailedPrecondition);
    assert!(
        err.message().contains("envelope"),
        "expected 'envelope' in message, got {:?}",
        err.message()
    );
    // Within the ±1 kW intersection is accepted.
    let resp = c
        .set_electrical_component_power(SetElectricalComponentPowerRequest {
            electrical_component_id: 4,
            power: 800.0,
            power_type: PowerType::Active as i32,
            request_lifetime: Some(30),
        })
        .await
        .expect("800 W is within the intersection");
    assert_eq!(
        resp.into_inner().message().await.unwrap().unwrap().status,
        SetElectricalComponentPowerRequestStatus::Success as i32,
    );
}

/// 0 W (the fail-safe park) must always be accepted, even when an
/// augmentation has narrowed the envelope to exclude it.
#[tokio::test(flavor = "multi_thread")]
async fn zero_power_is_always_allowed() {
    let s = TestServer::start(TINY_TOPOLOGY).await;
    let mut c = connect(&s).await;
    // Narrow inverter 4 to discharge-only [-5 kW, -1 kW], excluding 0 W.
    c.augment_electrical_component_bounds(AugmentElectricalComponentBoundsRequest {
        electrical_component_id: 4,
        target_metric: Metric::AcPowerActive as i32,
        bounds: vec![Bounds {
            lower: Some(-5000.0),
            upper: Some(-1000.0),
        }],
        request_lifetime: Some(30),
    })
    .await
    .expect("augment ok");

    // A non-zero setpoint outside the augmented band is still rejected...
    let err = c
        .set_electrical_component_power(SetElectricalComponentPowerRequest {
            electrical_component_id: 4,
            power: 500.0,
            power_type: PowerType::Active as i32,
            request_lifetime: Some(30),
        })
        .await
        .expect_err("500 W is outside the augmented envelope");
    assert_eq!(err.code(), tonic::Code::FailedPrecondition);

    // ...but 0 W is accepted regardless.
    let resp = c
        .set_electrical_component_power(SetElectricalComponentPowerRequest {
            electrical_component_id: 4,
            power: 0.0,
            power_type: PowerType::Active as i32,
            request_lifetime: Some(30),
        })
        .await
        .expect("0 W must be accepted");
    let first = resp
        .into_inner()
        .message()
        .await
        .expect("stream poll")
        .expect("a status");
    assert_eq!(
        first.status,
        SetElectricalComponentPowerRequestStatus::Success as i32,
    );
}

/// A malformed augmentation — inverted, or disjoint from the component's
/// bounds — must be rejected, not silently brick the component (every
/// setpoint then rejected while the running output goes unconstrained).
#[tokio::test(flavor = "multi_thread")]
async fn malformed_augmentation_is_rejected() {
    let s = TestServer::start(TINY_TOPOLOGY).await;
    let mut c = connect(&s).await;

    let inverted = c
        .augment_electrical_component_bounds(AugmentElectricalComponentBoundsRequest {
            electrical_component_id: 4,
            target_metric: Metric::AcPowerActive as i32,
            bounds: vec![Bounds {
                lower: Some(1000.0),
                upper: Some(-1000.0),
            }],
            request_lifetime: Some(30),
        })
        .await
        .expect_err("inverted bounds must be rejected");
    assert_eq!(inverted.code(), tonic::Code::InvalidArgument);

    // Disjoint from the inverter's rated ±5 kW band.
    let disjoint = c
        .augment_electrical_component_bounds(AugmentElectricalComponentBoundsRequest {
            electrical_component_id: 4,
            target_metric: Metric::AcPowerActive as i32,
            bounds: vec![Bounds {
                lower: Some(50_000.0),
                upper: Some(60_000.0),
            }],
            request_lifetime: Some(30),
        })
        .await
        .expect_err("disjoint bounds must be rejected");
    assert_eq!(disjoint.code(), tonic::Code::InvalidArgument);

    // A valid tightening still succeeds.
    c.augment_electrical_component_bounds(AugmentElectricalComponentBoundsRequest {
        electrical_component_id: 4,
        target_metric: Metric::AcPowerActive as i32,
        bounds: vec![Bounds {
            lower: Some(-2000.0),
            upper: Some(2000.0),
        }],
        request_lifetime: Some(30),
    })
    .await
    .expect("valid augmentation must be accepted");
}

/// Same shape as `TINY_TOPOLOGY`, but the inverter carries a real
/// reactive envelope: no PF limit (the inherited default would pin Q
/// to 0 at idle) and a 5 kVA apparent-power cap, so its Q band at
/// P = 0 is ±5 kVAr.
const REACTIVE_TOPOLOGY: &str = r#"
(%make-grid-connection-point :id 1
            :successors
            (list (%make-meter :id 2
                               :successors
                               (list (%make-battery-inverter
                                      :id 4
                                      :rated-lower -5000.0
                                      :rated-upper  5000.0
                                      :reactive-pf-limit 0
                                      :reactive-apparent-va 5000.0
                                      :successors
                                      (list (%make-battery
                                             :id 3
                                             :rated-lower -5000.0
                                             :rated-upper  5000.0)))))))
"#;

/// An `AC_POWER_REACTIVE` augmentation is accepted end-to-end: the
/// response carries the expiry the augmentation was armed with, and
/// the live telemetry stream reports the narrowed Q band.
#[tokio::test(flavor = "multi_thread")]
async fn reactive_augmentation_is_accepted_and_narrows_the_stream() {
    let s = TestServer::start(REACTIVE_TOPOLOGY).await;
    let mut c = connect(&s).await;

    // Baseline: the un-augmented band is the ±5 kVAr apparent cap.
    let wide = first_reactive_bounds(&mut c, 4).await;
    assert_eq!(wide.len(), 1, "expected one band, got {wide:?}");
    assert_eq!(wide[0].lower, Some(-5000.0));
    assert_eq!(wide[0].upper, Some(5000.0));

    let resp = c
        .augment_electrical_component_bounds(AugmentElectricalComponentBoundsRequest {
            electrical_component_id: 4,
            target_metric: Metric::AcPowerReactive as i32,
            bounds: vec![Bounds {
                lower: Some(-1000.0),
                upper: Some(1000.0),
            }],
            request_lifetime: Some(30),
        })
        .await
        .expect("a reactive augmentation must be accepted")
        .into_inner();
    assert!(
        resp.valid_until_time.is_some(),
        "an accepted augmentation reports when it expires",
    );

    // The journal keeps the axis: a Q augmentation is logged under
    // its own kind, not the active route's `augment_bounds`.
    let logged = s
        .config
        .site()
        .setpoints_window(4, chrono::Utc::now() - chrono::Duration::minutes(1));
    let kinds: Vec<&str> = logged.iter().map(|e| e.kind.as_str()).collect();
    assert!(
        kinds.contains(&"augment_reactive_bounds"),
        "expected the reactive augment kind in the journal, got {kinds:?}",
    );
    assert!(
        !kinds.contains(&"augment_bounds"),
        "the active augment kind must not be used for a Q request, got {kinds:?}",
    );

    let narrowed = first_reactive_bounds(&mut c, 4).await;
    assert_eq!(narrowed.len(), 1, "expected one band, got {narrowed:?}");
    assert_eq!(narrowed[0].lower, Some(-1000.0));
    assert_eq!(narrowed[0].upper, Some(1000.0));

    // The gateway now rejects a setpoint the caps band alone allowed.
    let err = c
        .set_electrical_component_power(SetElectricalComponentPowerRequest {
            electrical_component_id: 4,
            power: 3000.0,
            power_type: PowerType::Reactive as i32,
            request_lifetime: Some(30),
        })
        .await
        .expect_err("3 kVAr is outside the augmented ±1 kVAr band");
    assert_eq!(err.code(), tonic::Code::FailedPrecondition);
}

/// Zero Q headroom must not be a loophole in the augment gate.
///
/// The disjoint check runs against the component's LIVE Q envelope.
/// Telemetry normalizes a genuinely empty envelope to a present
/// `(0, 0)` band (`VecBounds::or_zero_band`) so consumers see "zero
/// headroom" rather than an absent bound — but if the gate saw that
/// normalized band it would accept any augmentation straddling zero,
/// leaving two live, mutually disjoint augmentations on the axis. So
/// the gate reads the RAW envelope, and an empty one is disjoint from
/// everything.
#[tokio::test(flavor = "multi_thread")]
async fn a_disjoint_q_augmentation_is_rejected_at_zero_headroom() {
    let s = TestServer::start(REACTIVE_TOPOLOGY).await;
    let mut c = connect(&s).await;

    // 1. At P = 0 the caps band is ±5 kVAr, so a [-4, -3] kVAr
    //    augmentation overlaps it and is accepted.
    c.augment_electrical_component_bounds(AugmentElectricalComponentBoundsRequest {
        electrical_component_id: 4,
        target_metric: Metric::AcPowerReactive as i32,
        bounds: vec![Bounds {
            lower: Some(-4000.0),
            upper: Some(-3000.0),
        }],
        request_lifetime: Some(30),
    })
    .await
    .expect("an augmentation overlapping the idle caps band is accepted");

    // 2. Drive P to the 5 kVA rim. The caps band collapses to (0, 0),
    //    which no longer overlaps the live augmentation: the real Q
    //    envelope is now EMPTY.
    c.set_electrical_component_power(SetElectricalComponentPowerRequest {
        electrical_component_id: 4,
        power: 5000.0,
        power_type: PowerType::Active as i32,
        request_lifetime: Some(30),
    })
    .await
    .expect("5 kW is inside the inverter's rated band");
    wait_for_active_power(&mut c, 4, 4200.0).await;

    // 3. [-500, 500] straddles zero, so it overlaps the NORMALIZED
    //    (0, 0) band telemetry publishes — but it is disjoint from the
    //    live [-4000, -3000] augmentation, and accepting it would leave
    //    the axis with two live augmentations that exclude each other.
    let err = c
        .augment_electrical_component_bounds(AugmentElectricalComponentBoundsRequest {
            electrical_component_id: 4,
            target_metric: Metric::AcPowerReactive as i32,
            bounds: vec![Bounds {
                lower: Some(-500.0),
                upper: Some(500.0),
            }],
            request_lifetime: Some(30),
        })
        .await
        .expect_err("an augmentation disjoint from the live Q envelope must be rejected");
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
    assert!(
        err.message().contains("disjoint"),
        "expected 'disjoint' in message, got {:?}",
        err.message(),
    );

    // 4. Nothing was stored: the axis still reports the honest zero
    //    headroom, and every nonzero Q setpoint is still refused.
    let band = first_reactive_bounds(&mut c, 4).await;
    assert_eq!(band.len(), 1, "expected one band, got {band:?}");
    assert_eq!((band[0].lower, band[0].upper), (Some(0.0), Some(0.0)));
    let err = c
        .set_electrical_component_power(SetElectricalComponentPowerRequest {
            electrical_component_id: 4,
            power: 400.0,
            power_type: PowerType::Reactive as i32,
            request_lifetime: Some(30),
        })
        .await
        .expect_err("no Q is legal with zero headroom");
    assert_eq!(err.code(), tonic::Code::FailedPrecondition);
}

/// Same as `REACTIVE_TOPOLOGY`, but the battery inverter's child is a
/// solar inverter, which *does* report a Q band (1 kVA cap -> ±1
/// kVAr). No production topology nests a Q-reporting child under an
/// inverter yet; this is how the reactive gateway's intersect branch
/// gets reached end-to-end.
const NESTED_REACTIVE_TOPOLOGY: &str = r#"
(%make-grid-connection-point :id 1
            :successors
            (list (%make-meter :id 2
                               :successors
                               (list (%make-battery-inverter
                                      :id 4
                                      :rated-lower -5000.0
                                      :rated-upper  5000.0
                                      :reactive-pf-limit 0
                                      :reactive-apparent-va 5000.0
                                      :successors
                                      (list (%make-solar-inverter
                                             :id 3
                                             :sunlight% 0
                                             :rated-lower -1000.0
                                             :rated-upper  0.0
                                             :reactive-pf-limit 0
                                             :reactive-apparent-va 1000.0)))))))
"#;

/// The SetPower gateway gates the reactive axis against the combined
/// Q envelope, mirroring the active axis — right down to the message,
/// which reports VAr rather than W.
#[tokio::test(flavor = "multi_thread")]
async fn set_reactive_power_outside_the_combined_envelope_is_rejected() {
    let s = TestServer::start(NESTED_REACTIVE_TOPOLOGY).await;
    let mut c = connect(&s).await;
    // 3 kVAr is inside the inverter's own ±5 kVAr but outside the
    // ±1 kVAr intersection with its Q-reporting child.
    let err = c
        .set_electrical_component_power(SetElectricalComponentPowerRequest {
            electrical_component_id: 4,
            power: 3000.0,
            power_type: PowerType::Reactive as i32,
            request_lifetime: Some(30),
        })
        .await
        .expect_err("expected rejection against the ±1 kVAr intersection");
    assert_eq!(err.code(), tonic::Code::FailedPrecondition);
    assert!(
        err.message().contains("combined envelope") && err.message().contains("VAr"),
        "expected the combined-envelope / VAr wording, got {:?}",
        err.message(),
    );
    // Inside the intersection is accepted, and so is the 0 VAr park.
    for power in [800.0, 0.0] {
        let resp = c
            .set_electrical_component_power(SetElectricalComponentPowerRequest {
                electrical_component_id: 4,
                power,
                power_type: PowerType::Reactive as i32,
                request_lifetime: Some(30),
            })
            .await
            .unwrap_or_else(|e| panic!("{power} VAr must be accepted, got {e:?}"));
        assert_eq!(
            resp.into_inner().message().await.unwrap().unwrap().status,
            SetElectricalComponentPowerRequestStatus::Success as i32,
        );
    }
}

/// Only the two AC power axes are augmentable. Anything else keeps
/// the invalid-argument rejection, and the message names the metric
/// that was asked for so a client can see what it got wrong.
#[tokio::test(flavor = "multi_thread")]
async fn augment_rejects_an_unsupported_metric_by_name() {
    let s = TestServer::start(REACTIVE_TOPOLOGY).await;
    let mut c = connect(&s).await;
    let err = c
        .augment_electrical_component_bounds(AugmentElectricalComponentBoundsRequest {
            electrical_component_id: 4,
            target_metric: Metric::DcPower as i32,
            bounds: vec![Bounds {
                lower: Some(-1000.0),
                upper: Some(1000.0),
            }],
            request_lifetime: Some(30),
        })
        .await
        .expect_err("DC_POWER bounds are not augmentable");
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
    assert!(
        err.message().contains("DC_POWER"),
        "expected the metric named in the message, got {:?}",
        err.message(),
    );
}

/// A reactive augmentation on a component with no Q axis is ACKed as
/// a no-op — the same gateway behavior the active side has for
/// axis-less components (todo #1007), chosen deliberately in the
/// design spec rather than inherited by accident. The ACK carries an
/// expiry, and the component's telemetry never grows a Q band.
#[tokio::test(flavor = "multi_thread")]
async fn reactive_augmentation_on_a_q_less_component_is_acked_as_a_no_op() {
    let s = TestServer::start(REACTIVE_TOPOLOGY).await;
    let mut c = connect(&s).await;
    let resp = c
        .augment_electrical_component_bounds(AugmentElectricalComponentBoundsRequest {
            electrical_component_id: 3, // the battery: no reactive axis
            target_metric: Metric::AcPowerReactive as i32,
            bounds: vec![Bounds {
                lower: Some(-1000.0),
                upper: Some(1000.0),
            }],
            request_lifetime: Some(30),
        })
        .await
        .expect("a Q augmentation on a Q-less component is ACKed")
        .into_inner();
    assert!(resp.valid_until_time.is_some());

    // Nothing was stored: the battery's stream stays free of reactive
    // bounds. Read a handful of samples rather than waiting out a
    // negative timeout.
    let mut stream = c
        .receive_electrical_component_telemetry_stream(
            ReceiveElectricalComponentTelemetryStreamRequest {
                electrical_component_id: 3,
                filter: None,
            },
        )
        .await
        .expect("subscribe")
        .into_inner();
    for _ in 0..3 {
        let msg = tokio::time::timeout(std::time::Duration::from_secs(5), stream.message())
            .await
            .expect("telemetry stream timed out")
            .expect("stream open")
            .expect("a sample");
        assert!(
            reactive_sample_bounds(&msg).is_none_or(|b| b.is_empty()),
            "a Q-less component must not grow reactive bounds from an ACKed no-op",
        );
    }
}

/// The reactive route feeds client input into `PowerAxis::augment`
/// just like the active one, so it must inherit the same shape
/// checks. A NaN edge is rejected rather than stored as a de-facto
/// no-op — and the Q band stays exactly where it was.
#[tokio::test(flavor = "multi_thread")]
async fn malformed_reactive_augmentation_is_rejected() {
    let s = TestServer::start(REACTIVE_TOPOLOGY).await;
    let mut c = connect(&s).await;

    let nan = c
        .augment_electrical_component_bounds(AugmentElectricalComponentBoundsRequest {
            electrical_component_id: 4,
            target_metric: Metric::AcPowerReactive as i32,
            bounds: vec![Bounds {
                lower: Some(f32::NAN),
                upper: Some(1000.0),
            }],
            request_lifetime: Some(30),
        })
        .await
        .expect_err("a non-finite edge must be rejected");
    assert_eq!(nan.code(), tonic::Code::InvalidArgument);
    assert!(
        nan.message().contains("non-finite"),
        "expected 'non-finite' in message, got {:?}",
        nan.message(),
    );

    // Inverted, and disjoint from the ±5 kVAr caps band.
    for bounds in [
        Bounds {
            lower: Some(1000.0),
            upper: Some(-1000.0),
        },
        Bounds {
            lower: Some(20_000.0),
            upper: Some(30_000.0),
        },
    ] {
        let err = c
            .augment_electrical_component_bounds(AugmentElectricalComponentBoundsRequest {
                electrical_component_id: 4,
                target_metric: Metric::AcPowerReactive as i32,
                bounds: vec![bounds],
                request_lifetime: Some(30),
            })
            .await
            .expect_err("malformed reactive bounds must be rejected");
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }

    // Nothing was stored: the band is still the full ±5 kVAr cap, not
    // a silently unconstrained or bricked axis.
    let band = first_reactive_bounds(&mut c, 4).await;
    assert_eq!(band.len(), 1, "expected one band, got {band:?}");
    assert_eq!(band[0].lower, Some(-5000.0));
    assert_eq!(band[0].upper, Some(5000.0));
}

#[tokio::test(flavor = "multi_thread")]
async fn telemetry_stream_emits_samples_for_a_component() {
    let s = TestServer::start(TINY_TOPOLOGY).await;
    let mut c = connect(&s).await;
    let resp = c
        .receive_electrical_component_telemetry_stream(
            ReceiveElectricalComponentTelemetryStreamRequest {
                electrical_component_id: 2, // main meter
                filter: None,
            },
        )
        .await
        .expect("subscribe");
    let mut stream = resp.into_inner();
    let mut got = 0usize;
    let take = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while let Ok(Some(msg)) = stream.message().await {
            if msg.telemetry.is_some() {
                got += 1;
                if got >= 2 {
                    break;
                }
            }
        }
    })
    .await;
    assert!(take.is_ok(), "stream timed out before 2 samples");
    assert!(got >= 2, "expected ≥2 samples, got {got}");
}
