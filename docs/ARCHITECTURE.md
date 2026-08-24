# Architecture

## Dependency rule

```text
Slint GUI ──> BackendClient ──> contract <── BackendService <── daemon/IPC
                                            ^
                                            |
                                      core orchestration
                                            |
                          audio / ASR / LLM / storage / platform traits
                                            |
                                  per-OS adapter implementations
```

The contract is the seam. The GUI cannot call capture, transcription, storage
or OS APIs directly. The backend cannot import Slint or GUI view models.

## Runtime topology

The production topology is two processes:

- `speakiput`: native Slint UI, view models and an IPC client;
- `speakiputd`: state owner, audio pipeline and platform integration.

The GUI opens a persistent duplex connection, negotiates a protocol version,
requests a snapshot and then consumes ordered events. A disconnected GUI can
reconnect without interrupting an active recording. A restarted backend gets a
new `instance_id`; the GUI discards event sequence assumptions from the old
instance and requests a fresh snapshot.

## Test topology

The same GUI uses an `InMemoryBackendClient` that returns canonical responses
and scripted event streams. The backend is driven by a conformance client and
fake implementations of microphone, ASR, LLM, storage and platform effects.
No test-only branch is permitted in production view models or core logic.

The intended Rust boundaries are:

```rust
trait BackendClient {
    async fn request(&self, request: Request) -> Result<Response, ClientError>;
    fn subscribe(&self) -> EventStream;
}

trait BackendService {
    async fn handle(&self, request: Request, client: ClientContext)
        -> Result<Response, ProtocolError>;
}
```

These signatures are illustrative; the exact async abstraction is chosen when
the workspace is scaffolded. The semantic boundary in `contracts/v1` is fixed.

## Domain state

The backend state progression for dictation is:

```text
idle -> recording -> transcribing -> post_processing -> injecting -> idle
```

`post_processing` is skipped when disabled; `injecting` is skipped for
clipboard-only/output-only modes. Failures return to `idle` after emitting a
terminal session/error event. Commands validate the current state, making
duplicate starts/stops safe and observable.

## Platform boundary

Platform traits cover only effects that differ by OS:

- enumerate/select audio inputs when CPAL does not provide enough information;
- register global shortcuts;
- identify/refocus the target application;
- insert text or copy it to the clipboard;
- tray, notifications, autostart and permissions.

Linux implementations can reuse whisrs behavior. Windows and macOS receive new
adapters without modifying the contract or core state machine.
