//! TimeoutTracker integration test. A setpoint with a short request
//! lifetime runs out; the gateway's physics step expires it and the
//! ramp slews back to idle.

mod common;

use std::time::Duration;

use common::TestServer;
use macrocosim::proto::microgrid::microgrid_client::MicrogridClient;
use macrocosim::proto::microgrid::{PowerType, SetElectricalComponentPowerRequest};

/// The setpoint's request lifetime, in seconds.
const LIFETIME_S: u64 = 10;

const INVERTER_AND_BATTERY: &str = r#"{
  "components": [
    {"type": "battery", "id": 100, "capacity_wh": 100000.0,
     "rated_lower_w": -10000.0, "rated_upper_w": 10000.0},
    {"type": "battery_inverter", "id": 200,
     "rated_lower_w": -10000.0, "rated_upper_w": 10000.0}
  ],
  "connections": [[200, 100]]
}"#;

#[tokio::test(flavor = "multi_thread")]
async fn short_lifetime_setpoint_resets_after_expiry() {
    let s = TestServer::start_mg(INVERTER_AND_BATTERY).await;

    // Apply a non-zero setpoint with the shortest lifetime the
    // Microgrid API allows for SetElectricalComponentPower, 10 s.
    let mut c = MicrogridClient::connect(s.grpc_url.clone())
        .await
        .expect("grpc connect");
    let sent = tokio::time::Instant::now();
    c.set_electrical_component_power(SetElectricalComponentPowerRequest {
        electrical_component_id: 200,
        power: 3000.0,
        power_type: PowerType::Active as i32,
        request_lifetime: Some(LIFETIME_S),
    })
    .await
    .expect("set-power accepted");

    // Step 1: confirm the setpoint took effect. The command crosses
    // the gateway and device delays, so poll against a deadline.
    let inv = s.site().get(200).unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let p_before = loop {
        s.site()
            .tick_once(chrono::Utc::now(), Duration::from_millis(100));
        let p = inv
            .telemetry(&s.site())
            .active_power_w
            .expect("active power present");
        if (p - 3000.0).abs() < 1.0 || tokio::time::Instant::now() >= deadline {
            break p;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert!(
        (p_before - 3000.0).abs() < 1.0,
        "expected 3000 W after setpoint, got {p_before}",
    );

    // Step 2+3: wait for the gateway's physics step to expire the
    // entry and reset the setpoint, then tick so the ramp lands at 0.
    // Polling with a deadline instead of one fixed sleep keeps a
    // stalled CI machine from failing the test spuriously. With an
    // instant ramp, one tick after the reset is enough.
    let deadline = sent + Duration::from_secs(LIFETIME_S + 10);
    let p_after = loop {
        tokio::time::sleep(Duration::from_millis(50)).await;
        s.site()
            .tick_once(chrono::Utc::now(), Duration::from_millis(100));
        let p = inv
            .telemetry(&s.site())
            .active_power_w
            .expect("active power present");
        if p.abs() < 1.0 || tokio::time::Instant::now() >= deadline {
            break p;
        }
    };
    assert!(
        p_after.abs() < 1.0,
        "expected setpoint reset to 0 W, got {p_after}",
    );
}
