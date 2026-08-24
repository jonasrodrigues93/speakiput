//! Deterministic fake backend and shared protocol conformance harness.

use std::{collections::VecDeque, sync::Arc, time::Duration};

use async_trait::async_trait;
use serde_json::Value;
use speakiput_client::{BackendService, InMemoryBackendClient, ServiceOutput};
use speakiput_contract::{Envelope, ProtocolError};
use tokio::sync::Mutex;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct EventTemplate {
    pub name: String,
    pub payload: Value,
}

impl EventTemplate {
    #[must_use]
    pub fn new(name: impl Into<String>, payload: Value) -> Self {
        Self {
            name: name.into(),
            payload,
        }
    }
}

#[derive(Debug, Clone)]
pub enum ScriptedResult {
    Success(Value),
    Error(ProtocolError),
}

#[derive(Debug, Clone)]
pub struct ScriptedStep {
    pub expected_name: String,
    pub result: ScriptedResult,
    pub events: Vec<EventTemplate>,
    pub delay: Duration,
}

impl ScriptedStep {
    #[must_use]
    pub fn success(name: impl Into<String>, payload: Value) -> Self {
        Self {
            expected_name: name.into(),
            result: ScriptedResult::Success(payload),
            events: Vec::new(),
            delay: Duration::ZERO,
        }
    }

    #[must_use]
    pub fn error(name: impl Into<String>, error: ProtocolError) -> Self {
        Self {
            expected_name: name.into(),
            result: ScriptedResult::Error(error),
            events: Vec::new(),
            delay: Duration::ZERO,
        }
    }

    #[must_use]
    pub fn with_event(mut self, name: impl Into<String>, payload: Value) -> Self {
        self.events.push(EventTemplate::new(name, payload));
        self
    }

    #[must_use]
    pub fn with_delay(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }
}

#[derive(Debug)]
struct ScriptState {
    steps: VecDeque<ScriptedStep>,
    sequence: u64,
}

#[derive(Debug)]
pub struct ScriptedFakeBackend {
    instance_id: Uuid,
    state: Mutex<ScriptState>,
}

impl ScriptedFakeBackend {
    #[must_use]
    pub fn new(steps: impl IntoIterator<Item = ScriptedStep>) -> Self {
        Self {
            instance_id: Uuid::new_v4(),
            state: Mutex::new(ScriptState {
                steps: steps.into_iter().collect(),
                sequence: 0,
            }),
        }
    }

    #[must_use]
    pub fn instance_id(&self) -> Uuid {
        self.instance_id
    }

    pub async fn remaining_steps(&self) -> usize {
        self.state.lock().await.steps.len()
    }
}

#[async_trait]
impl BackendService for ScriptedFakeBackend {
    async fn handle(&self, request: Envelope) -> Result<ServiceOutput, String> {
        let step = {
            let mut state = self.state.lock().await;
            state.steps.pop_front().ok_or_else(|| {
                format!("unexpected request {}; script is exhausted", request.name)
            })?
        };

        if step.expected_name != request.name {
            return Err(format!(
                "expected request {}, received {}",
                step.expected_name, request.name
            ));
        }
        if !step.delay.is_zero() {
            tokio::time::sleep(step.delay).await;
        }

        let response = match step.result {
            ScriptedResult::Success(payload) => {
                Envelope::response(&request, self.instance_id, payload)
            }
            ScriptedResult::Error(error) => {
                Envelope::error_response(&request, self.instance_id, error)
            }
        };
        let mut state = self.state.lock().await;
        let events = step
            .events
            .into_iter()
            .map(|template| {
                state.sequence += 1;
                Envelope::event(
                    template.name,
                    self.instance_id,
                    state.sequence,
                    template.payload,
                )
            })
            .collect();
        Ok(ServiceOutput::response(response).with_events(events))
    }
}

#[must_use]
pub fn scripted_client(
    steps: impl IntoIterator<Item = ScriptedStep>,
) -> (InMemoryBackendClient, Arc<ScriptedFakeBackend>) {
    let backend = Arc::new(ScriptedFakeBackend::new(steps));
    let service: Arc<dyn BackendService> = backend.clone();
    (InMemoryBackendClient::connect(service), backend)
}

pub mod conformance;
