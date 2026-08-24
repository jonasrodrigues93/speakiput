use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

use speakiput_client::BackendService;
use speakiput_contract::{Envelope, MAX_FRAME_BYTES, MessageKind, decode_frame, encode_frame};
use thiserror::Error;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream},
    sync::{broadcast, mpsc},
};
use tracing::{debug, warn};

#[derive(Debug, Error)]
pub enum ServerError {
    #[error("IPC I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("another speakiputd instance is already listening at {0}")]
    AlreadyRunning(PathBuf),
}

pub async fn serve_until<F>(
    path: impl AsRef<Path>,
    service: Arc<dyn BackendService>,
    shutdown: F,
) -> Result<(), ServerError>
where
    F: Future<Output = ()>,
{
    let path = path.as_ref().to_owned();
    prepare_socket(&path).await?;
    let listener = UnixListener::bind(&path)?;
    set_owner_only(&path)?;
    let _guard = SocketGuard(path.clone());
    let (events, _) = broadcast::channel::<Envelope>(256);
    if let Some(mut service_events) = service.subscribe() {
        let events = events.clone();
        tokio::spawn(async move {
            loop {
                match service_events.recv().await {
                    Ok(event) => {
                        let _ = events.send(event);
                    }
                    Err(broadcast::error::RecvError::Lagged(count)) => {
                        warn!(count, "daemon event bridge lagged");
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
        });
    }
    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            biased;
            () = &mut shutdown => return Ok(()),
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                let service = Arc::clone(&service);
                let events = events.clone();
                tokio::spawn(async move {
                    if let Err(error) = handle_connection(stream, service, events).await {
                        debug!(%error, "IPC client disconnected");
                    }
                });
            }
        }
    }
}

async fn prepare_socket(path: &Path) -> Result<(), ServerError> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    if tokio::fs::try_exists(path).await? {
        if UnixStream::connect(path).await.is_ok() {
            return Err(ServerError::AlreadyRunning(path.to_owned()));
        }
        tokio::fs::remove_file(path).await?;
    }
    Ok(())
}

#[cfg(unix)]
fn set_owner_only(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            let parent_mode = path
                .parent()
                .and_then(|parent| std::fs::metadata(parent).ok())
                .map_or(0o777, |metadata| metadata.permissions().mode());
            if parent_mode.trailing_zeros() >= 6 {
                warn!(path = %path.display(), "socket chmod unavailable; owner-only parent directory still protects it");
                Ok(())
            } else {
                Err(error)
            }
        }
        Err(error) => Err(error),
    }
}

async fn handle_connection(
    stream: UnixStream,
    service: Arc<dyn BackendService>,
    events: broadcast::Sender<Envelope>,
) -> Result<(), String> {
    let (mut reader, mut writer) = stream.into_split();
    let (outgoing, mut outgoing_rx) = mpsc::channel::<Envelope>(64);
    let mut event_rx = events.subscribe();
    let event_outgoing = outgoing.clone();
    let event_task = tokio::spawn(async move {
        loop {
            match event_rx.recv().await {
                Ok(event) => {
                    if event_outgoing.send(event).await.is_err() {
                        return;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(count)) => {
                    warn!(count, "disconnecting lagged IPC event subscriber");
                    return;
                }
                Err(broadcast::error::RecvError::Closed) => return,
            }
        }
    });
    let writer_task = tokio::spawn(async move {
        while let Some(envelope) = outgoing_rx.recv().await {
            let frame = encode_frame(&envelope).map_err(|error| error.to_string())?;
            writer
                .write_all(&frame)
                .await
                .map_err(|error| error.to_string())?;
        }
        writer.shutdown().await.map_err(|error| error.to_string())
    });

    let result = async {
        let mut greeted = false;
        loop {
            let request = read_async_frame(&mut reader).await?;
            if request.kind != MessageKind::Request {
                return Err("client sent a non-request envelope".into());
            }
            if !greeted && request.name != "client.hello" {
                return Err("client.hello must be the first request on a connection".into());
            }
            let output = service.handle(request).await?;
            let accepted_hello =
                output.response.name == "client.hello" && output.response.error.is_none();
            outgoing
                .send(output.response)
                .await
                .map_err(|_| "IPC writer closed".to_owned())?;
            if accepted_hello {
                greeted = true;
            }
            for event in output.events {
                let _ = events.send(event);
            }
        }
    }
    .await;
    drop(outgoing);
    event_task.abort();
    writer_task.abort();
    result
}

async fn read_async_frame(reader: &mut (impl AsyncRead + Unpin)) -> Result<Envelope, String> {
    let mut header = [0_u8; 4];
    reader
        .read_exact(&mut header)
        .await
        .map_err(|error| error.to_string())?;
    let length = u32::from_be_bytes(header) as usize;
    if length > MAX_FRAME_BYTES {
        return Err(format!("frame is too large: {length}"));
    }
    let mut payload = vec![0; length];
    reader
        .read_exact(&mut payload)
        .await
        .map_err(|error| error.to_string())?;
    let mut frame = Vec::with_capacity(length + 4);
    frame.extend_from_slice(&header);
    frame.extend_from_slice(&payload);
    decode_frame(&frame).map_err(|error| error.to_string())
}

struct SocketGuard(PathBuf);

impl Drop for SocketGuard {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_file(&self.0)
            && error.kind() != io::ErrorKind::NotFound
        {
            warn!(%error, path = %self.0.display(), "failed to remove IPC socket");
        }
    }
}
