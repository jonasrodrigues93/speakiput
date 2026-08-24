//! GUI-facing backend client abstraction and transports used by production and tests.

use std::sync::Arc;

use async_trait::async_trait;
use speakiput_contract::{Envelope, MessageKind, ProtocolError};
use thiserror::Error;
use tokio::sync::{broadcast, mpsc, oneshot};

#[cfg(unix)]
mod unix;

#[cfg(unix)]
pub use unix::{UnixBackendClient, default_socket_path};

#[derive(Debug, Error)]
pub enum ClientError {
    #[error("backend client transport is closed")]
    TransportClosed,
    #[error("backend rejected the request: {0:?}")]
    Protocol(ProtocolError),
    #[error("backend service failed: {0}")]
    Service(String),
    #[error("invalid response: {0}")]
    InvalidResponse(String),
    #[error("event subscriber lagged by {0} events")]
    SubscriptionLagged(u64),
}

#[async_trait]
pub trait BackendClient: Send + Sync {
    async fn request(&self, request: Envelope) -> Result<Envelope, ClientError>;
    fn subscribe(&self) -> EventSubscription;
}

#[derive(Debug)]
pub struct EventSubscription {
    receiver: broadcast::Receiver<Envelope>,
}

impl EventSubscription {
    pub async fn recv(&mut self) -> Result<Envelope, ClientError> {
        match self.receiver.recv().await {
            Ok(event) => Ok(event),
            Err(broadcast::error::RecvError::Closed) => Err(ClientError::TransportClosed),
            Err(broadcast::error::RecvError::Lagged(count)) => {
                Err(ClientError::SubscriptionLagged(count))
            }
        }
    }
}

#[derive(Debug)]
pub struct ServiceOutput {
    pub response: Envelope,
    pub events: Vec<Envelope>,
}

impl ServiceOutput {
    #[must_use]
    pub fn response(response: Envelope) -> Self {
        Self {
            response,
            events: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_events(mut self, events: Vec<Envelope>) -> Self {
        self.events = events;
        self
    }
}

#[async_trait]
pub trait BackendService: Send + Sync + 'static {
    async fn handle(&self, request: Envelope) -> Result<ServiceOutput, String>;

    fn subscribe(&self) -> Option<broadcast::Receiver<Envelope>> {
        None
    }
}

struct RequestCommand {
    request: Envelope,
    reply: oneshot::Sender<Result<Envelope, ClientError>>,
}

#[derive(Clone)]
pub struct InMemoryBackendClient {
    requests: mpsc::Sender<RequestCommand>,
    events: broadcast::Sender<Envelope>,
}

impl InMemoryBackendClient {
    #[must_use]
    pub fn connect(service: Arc<dyn BackendService>) -> Self {
        let (requests, mut request_rx) = mpsc::channel::<RequestCommand>(64);
        let (events, _) = broadcast::channel(256);
        let event_sender = events.clone();

        if let Some(mut service_events) = service.subscribe() {
            let event_sender = event_sender.clone();
            tokio::spawn(async move {
                loop {
                    match service_events.recv().await {
                        Ok(event) => {
                            let _ = event_sender.send(event);
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => {}
                        Err(broadcast::error::RecvError::Closed) => return,
                    }
                }
            });
        }

        tokio::spawn(async move {
            while let Some(command) = request_rx.recv().await {
                let service = Arc::clone(&service);
                let events = event_sender.clone();
                tokio::spawn(async move {
                    let result = service
                        .handle(command.request.clone())
                        .await
                        .map_err(ClientError::Service)
                        .and_then(|output| validate_output(&command.request, output, &events));
                    let _ = command.reply.send(result);
                });
            }
        });

        Self { requests, events }
    }
}

fn validate_output(
    request: &Envelope,
    output: ServiceOutput,
    events: &broadcast::Sender<Envelope>,
) -> Result<Envelope, ClientError> {
    output
        .response
        .validate()
        .map_err(|error| ClientError::InvalidResponse(error.to_string()))?;
    if output.response.kind != MessageKind::Response
        || output.response.name != request.name
        || output.response.correlation_id != Some(request.message_id)
    {
        return Err(ClientError::InvalidResponse(
            "response name/correlation does not match request".into(),
        ));
    }
    for event in output.events {
        event
            .validate()
            .map_err(|error| ClientError::InvalidResponse(error.to_string()))?;
        if event.kind != MessageKind::Event {
            return Err(ClientError::InvalidResponse(
                "service emitted a non-event".into(),
            ));
        }
        let _ = events.send(event);
    }
    if let Some(error) = output.response.error.clone() {
        return Err(ClientError::Protocol(error));
    }
    Ok(output.response)
}

#[async_trait]
impl BackendClient for InMemoryBackendClient {
    async fn request(&self, request: Envelope) -> Result<Envelope, ClientError> {
        request
            .validate()
            .map_err(|error| ClientError::InvalidResponse(error.to_string()))?;
        if request.kind != MessageKind::Request {
            return Err(ClientError::InvalidResponse(
                "client can only send requests".into(),
            ));
        }
        let (reply, receiver) = oneshot::channel();
        self.requests
            .send(RequestCommand { request, reply })
            .await
            .map_err(|_| ClientError::TransportClosed)?;
        receiver.await.map_err(|_| ClientError::TransportClosed)?
    }

    fn subscribe(&self) -> EventSubscription {
        EventSubscription {
            receiver: self.events.subscribe(),
        }
    }
}
