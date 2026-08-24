use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use speakiput_contract::{Envelope, MAX_FRAME_BYTES, MessageKind, decode_frame, encode_frame};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
    sync::{broadcast, mpsc, oneshot},
};
use uuid::Uuid;

use crate::{BackendClient, ClientError, EventSubscription};

type PendingRequests = Arc<Mutex<HashMap<Uuid, oneshot::Sender<Result<Envelope, ClientError>>>>>;

#[derive(Clone)]
pub struct UnixBackendClient {
    outgoing: mpsc::Sender<Envelope>,
    events: broadcast::Sender<Envelope>,
    pending: PendingRequests,
}

impl UnixBackendClient {
    pub async fn connect(path: impl AsRef<Path>) -> Result<Self, ClientError> {
        let stream = UnixStream::connect(path)
            .await
            .map_err(|error| ClientError::Service(error.to_string()))?;
        let (mut reader, mut writer) = stream.into_split();
        let (outgoing, mut outgoing_rx) = mpsc::channel::<Envelope>(64);
        let (events, _) = broadcast::channel(256);
        let pending: PendingRequests = Arc::new(Mutex::new(HashMap::new()));

        let writer_pending = Arc::clone(&pending);
        tokio::spawn(async move {
            while let Some(envelope) = outgoing_rx.recv().await {
                let frame = match encode_frame(&envelope) {
                    Ok(frame) => frame,
                    Err(error) => {
                        fail_one(&writer_pending, envelope.message_id, error.to_string());
                        continue;
                    }
                };
                if writer.write_all(&frame).await.is_err() {
                    fail_all(&writer_pending);
                    return;
                }
            }
            let _ = writer.shutdown().await;
            fail_all(&writer_pending);
        });

        let reader_pending = Arc::clone(&pending);
        let reader_events = events.clone();
        tokio::spawn(async move {
            loop {
                let Ok(envelope) = read_async_frame(&mut reader).await else {
                    fail_all(&reader_pending);
                    return;
                };
                match envelope.kind {
                    MessageKind::Response => {
                        if let Some(correlation_id) = envelope.correlation_id {
                            let sender = reader_pending
                                .lock()
                                .ok()
                                .and_then(|mut pending| pending.remove(&correlation_id));
                            if let Some(sender) = sender {
                                let result = envelope.error.clone().map_or_else(
                                    || Ok(envelope),
                                    |error| Err(ClientError::Protocol(error)),
                                );
                                let _ = sender.send(result);
                            }
                        }
                    }
                    MessageKind::Event => {
                        let _ = reader_events.send(envelope);
                    }
                    MessageKind::Request => {}
                }
            }
        });

        Ok(Self {
            outgoing,
            events,
            pending,
        })
    }
}

#[must_use]
pub fn default_socket_path() -> PathBuf {
    if let Some(runtime_dir) = std::env::var_os("XDG_RUNTIME_DIR") {
        return PathBuf::from(runtime_dir).join("speakiput/speakiputd.sock");
    }
    let cache_dir = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .unwrap_or_else(std::env::temp_dir);
    cache_dir.join("speakiput/runtime/speakiputd.sock")
}

#[async_trait]
impl BackendClient for UnixBackendClient {
    async fn request(&self, request: Envelope) -> Result<Envelope, ClientError> {
        request
            .validate()
            .map_err(|error| ClientError::InvalidResponse(error.to_string()))?;
        if request.kind != MessageKind::Request {
            return Err(ClientError::InvalidResponse(
                "client can only send requests".into(),
            ));
        }
        let request_id = request.message_id;
        let (reply, receiver) = oneshot::channel();
        self.pending
            .lock()
            .map_err(|_| ClientError::TransportClosed)?
            .insert(request_id, reply);
        if self.outgoing.send(request).await.is_err() {
            self.pending
                .lock()
                .ok()
                .and_then(|mut pending| pending.remove(&request_id));
            return Err(ClientError::TransportClosed);
        }
        receiver.await.map_err(|_| ClientError::TransportClosed)?
    }

    fn subscribe(&self) -> EventSubscription {
        EventSubscription {
            receiver: self.events.subscribe(),
        }
    }
}

async fn read_async_frame(reader: &mut (impl AsyncRead + Unpin)) -> Result<Envelope, ClientError> {
    let mut header = [0_u8; 4];
    reader
        .read_exact(&mut header)
        .await
        .map_err(|_| ClientError::TransportClosed)?;
    let length = u32::from_be_bytes(header) as usize;
    if length > MAX_FRAME_BYTES {
        return Err(ClientError::InvalidResponse(format!(
            "frame payload is {length} bytes; maximum is {MAX_FRAME_BYTES}"
        )));
    }
    let mut payload = vec![0; length];
    reader
        .read_exact(&mut payload)
        .await
        .map_err(|_| ClientError::TransportClosed)?;
    let mut frame = Vec::with_capacity(length + 4);
    frame.extend_from_slice(&header);
    frame.extend_from_slice(&payload);
    decode_frame(&frame).map_err(|error| ClientError::InvalidResponse(error.to_string()))
}

fn fail_one(pending: &PendingRequests, request_id: Uuid, message: String) {
    let sender = pending
        .lock()
        .ok()
        .and_then(|mut pending| pending.remove(&request_id));
    if let Some(sender) = sender {
        let _ = sender.send(Err(ClientError::Service(message)));
    }
}

fn fail_all(pending: &PendingRequests) {
    let senders = pending
        .lock()
        .map(|mut pending| {
            pending
                .drain()
                .map(|(_, sender)| sender)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    for sender in senders {
        let _ = sender.send(Err(ClientError::TransportClosed));
    }
}
