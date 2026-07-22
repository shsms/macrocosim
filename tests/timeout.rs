//! TimeoutTracker integration test. A setpoint with a short
//! request-lifetime expires; the timeout loop calls reset_setpoint
//! on the component and the ramp slews back to idle.

mod common;

use std::time::Duration;

use common::TestServer;

const INVERTER_AND_BATTERY: &str = r#"
(set-microgrid-id 9)
(setq b (%make-battery :id 100
                       :capacity 100000.0
                       :rated-lower -10000.0
                       :rated-upper  10000.0))
(%make-battery-inverter :id 200
                        :rated-lower -10000.0
                        :rated-upper  10000.0
                        :successors (list b))
"#;

#[tokio::test(flavor = "multi_thread")]
async fn short_lifetime_setpoint_resets_after_expiry() {
    let s = TestServer::start(INVERTER_AND_BATTERY).await;
    let client = reqwest::Client::new();

    // Apply a non-zero setpoint with a 2 s lifetime.
    // (set-active-power id watts lifetime-ms) — the Lisp defun
    // doesn't enforce the gRPC handler's 10 s minimum, so this
    // path lets us drive a fast expiry deterministically. The
    // lifetime must comfortably outlast the eval-to-first-assert
    // gap, or a loaded runner expires the setpoint before step 1
    // reads it back.
    let r = client
        .post(format!("{}/api/eval", s.ui_url))
        .body("(set-active-power 200 3000.0 2000)")
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success(), "set-active-power eval failed");

    // Step 1: confirm the setpoint took effect by ticking once and
    // reading active power back from the inverter's telemetry.
    let inv = s.config.site().get(200).unwrap();
    s.config
        .site()
        .tick_once(chrono::Utc::now(), Duration::from_millis(100));
    let p_before = inv
        .telemetry(&s.config.site())
        .active_power_w
        .expect("active power present");
    assert!(
        (p_before - 3000.0).abs() < 1.0,
        "expected 3000 W after setpoint, got {p_before}",
    );

    // Step 2+3: wait for the timeout loop (100 ms poll cadence) to
    // drain the expired entry and reset the setpoint, then tick so
    // the ramp lands at 0. Polling with a deadline instead of one
    // fixed sleep keeps a stalled CI machine from failing the test
    // spuriously. With infinite default ramp-rate, one tick after
    // the reset is enough.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let p_after = loop {
        tokio::time::sleep(Duration::from_millis(50)).await;
        s.config
            .site()
            .tick_once(chrono::Utc::now(), Duration::from_millis(100));
        let p = inv
            .telemetry(&s.config.site())
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
