use std::time::Duration;

use serde_json::json;
use speakiput_client::BackendClient;
use speakiput_contract::Envelope;
use speakiput_testing::{ScriptedStep, scripted_client};

#[tokio::test]
async fn correlates_concurrent_out_of_order_responses() {
    let steps = vec![
        ScriptedStep::success("state.get", json!({ "marker": "slow" }))
            .with_delay(Duration::from_millis(30)),
        ScriptedStep::success("settings.get", json!({ "marker": "fast" })),
    ];
    let (client, backend) = scripted_client(steps);
    let first = client.request(Envelope::request("state.get", json!({})));
    let second = client.request(Envelope::request("settings.get", json!({})));
    let (first, second) = tokio::join!(first, second);
    assert_eq!(first.unwrap().payload["marker"], "slow");
    assert_eq!(second.unwrap().payload["marker"], "fast");
    assert_eq!(backend.remaining_steps().await, 0);
}

#[tokio::test]
async fn emits_scripted_events_in_monotonic_order() {
    let step = ScriptedStep::success("recording.start", json!({ "accepted": true }))
        .with_event("state.changed", json!({ "current": "recording" }))
        .with_event("recording.level", json!({ "level": 0.3 }));
    let (client, _) = scripted_client([step]);
    let mut subscription = client.subscribe();
    client
        .request(Envelope::request("recording.start", json!({})))
        .await
        .unwrap();
    assert_eq!(subscription.recv().await.unwrap().sequence, Some(1));
    assert_eq!(subscription.recv().await.unwrap().sequence, Some(2));
}

#[tokio::test]
async fn shared_conformance_harness_passes() {
    speakiput_testing::conformance::run().await.unwrap();
}
