//! Smoke tests: spin up the API against a hand-rolled `ApiContext`
//! (no egui needed) and verify a few end-to-end round-trips.

use std::sync::{mpsc, Arc};
use std::time::Instant;

use arc_swap::ArcSwap;
use arion_app::protocol::{Action, StateSnapshot};
use arion_core::Telemetry;

fn fresh_ctx() -> (
    arion_api::ApiContext,
    mpsc::Receiver<Action>,
    Arc<ArcSwap<StateSnapshot>>,
) {
    let snapshot = Arc::new(ArcSwap::new(Arc::new(StateSnapshot {
        num_rx: 2,
        active_rx: 0,
        radio_connected: false,
        radio_ip: "192.168.1.40".into(),
        rx: vec![
            arion_app::protocol::RxSnapshot {
                enabled:      true,
                frequency_hz: 14_074_000,
                mode:         "USB",
                volume:       0.5,
                s_meter_db:   -90.0,
                nb: false, nb2: false, anf: false, bin: false, tnf: false,
                nr3: false, nr4: false, anr: false, emnr: false,
            },
            arion_app::protocol::RxSnapshot {
                enabled:      false,
                frequency_hz: 7_074_000,
                mode:         "USB",
                volume:       0.5,
                s_meter_db:   -140.0,
                nb: false, nb2: false, anf: false, bin: false, tnf: false,
                nr3: false, nr4: false, anr: false, emnr: false,
            },
        ],
        memories: vec![],
    })));
    let telemetry = Arc::new(ArcSwap::new(Arc::new(Telemetry::default())));
    let (action_tx, action_rx) = mpsc::channel();
    let last_event = Arc::new(ArcSwap::new(Arc::new(None)));
    let ctx = arion_api::ApiContext {
        snapshot:       snapshot.clone(),
        telemetry,
        action_tx,
        script_tx:      None,
        midi_mapping:   None,
        midi_last_event: last_event,
        midi_persist:   false,
        started_at:     Instant::now(),
        build_version:  "test",
        midi_persist_path: None,
    };
    (ctx, action_rx, snapshot)
}

#[tokio::test]
async fn instance_endpoint_returns_build_info() {
    let (ctx, _rx, _snap) = fresh_ctx();
    let handle = arion_api::start("127.0.0.1:0".parse().unwrap(), ctx).unwrap();
    let addr = handle.addr();

    let body: serde_json::Value = reqwest::get(format!("http://{addr}/api/v1/instance"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["name"], "arion");
    assert_eq!(body["num_rx"], 2);
    handle.stop();
}

#[tokio::test]
async fn patch_rx_emits_actions() {
    let (ctx, action_rx, _snap) = fresh_ctx();
    let handle = arion_api::start("127.0.0.1:0".parse().unwrap(), ctx).unwrap();
    let addr = handle.addr();
    let client = reqwest::Client::new();

    let resp = client
        .patch(format!("http://{addr}/api/v1/rx/0"))
        .json(&serde_json::json!({ "frequency_hz": 14074000, "mode": "USB" }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());

    let mut got_freq = false;
    let mut got_mode = false;
    for _ in 0..10 {
        if let Ok(a) = action_rx.try_recv() {
            match a {
                Action::SetRxFrequency { rx: 0, hz: 14_074_000 } => got_freq = true,
                Action::SetRxMode { rx: 0, mode } if mode == "USB" => got_mode = true,
                _ => {}
            }
        } else {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }
    assert!(got_freq, "expected SetRxFrequency action");
    assert!(got_mode, "expected SetRxMode action");
    handle.stop();
}

#[tokio::test]
async fn unknown_rx_returns_404() {
    let (ctx, _rx, _snap) = fresh_ctx();
    let handle = arion_api::start("127.0.0.1:0".parse().unwrap(), ctx).unwrap();
    let addr = handle.addr();

    let resp = reqwest::get(format!("http://{addr}/api/v1/rx/99")).await.unwrap();
    assert_eq!(resp.status().as_u16(), 404);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"], "not_found");
    handle.stop();
}

#[tokio::test]
async fn metrics_are_prometheus_formatted() {
    let (ctx, _rx, _snap) = fresh_ctx();
    let handle = arion_api::start("127.0.0.1:0".parse().unwrap(), ctx).unwrap();
    let addr = handle.addr();

    let resp = reqwest::get(format!("http://{addr}/api/v1/metrics")).await.unwrap();
    assert!(resp.status().is_success());
    let body = resp.text().await.unwrap();
    assert!(body.contains("arion_num_rx 2"));
    assert!(body.contains("arion_rx_frequency_hz{rx=\"0\"} 14074000"));
    handle.stop();
}
