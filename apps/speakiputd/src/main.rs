use std::sync::Arc;

use speakiput_client::{BackendClient, BackendService, UnixBackendClient};
use speakiput_contract::{
    AudioDevice, ClientHelloRequest, ClientIdentity, DictationState, Envelope, PROTOCOL_VERSION,
    RecordingStartRequest, RecordingStopRequest, SettingsResponse, StateSnapshot,
};
use speakiput_platform::{CapabilityReporter, ShortcutService};
#[cfg(target_os = "linux")]
use speakiput_platform_linux::{LinuxPlatform, LinuxShortcutService};
#[cfg(target_os = "macos")]
use speakiput_platform_macos::{MacPlatform, MacShortcutService};
use speakiput_storage::{
    JsonSettingsRepository, JsonlHistoryRepository, SettingsRepository, SystemCredentialRepository,
};
#[cfg(feature = "native")]
use speakiputd::service::RuntimeComponents;
use speakiputd::{
    default_socket_path, default_storage_paths, server::serve_until, service::SpeakiputService,
};
use tracing::{info, warn};

#[cfg(feature = "native")]
mod native;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args_os().any(|argument| argument == "--toggle-recording") {
        return toggle_recording().await;
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let (settings_path, history_path) = default_storage_paths();
    let settings_repository = Arc::new(JsonSettingsRepository::new(settings_path));
    let history_repository = Arc::new(JsonlHistoryRepository::new(history_path));
    let credential_repository = Arc::new(SystemCredentialRepository);
    #[cfg(target_os = "linux")]
    let platform = Arc::new(LinuxPlatform);
    #[cfg(target_os = "macos")]
    let platform = Arc::new(MacPlatform);
    let platform_capabilities = platform.capabilities();
    let mut capabilities = vec![
        "history".into(),
        "credential_store".into(),
        // The GUI overlay is a click-through, undecorated window. Wayland
        // compositors do not grant it activation without a user token, while
        // X11 sessions are restored below when a focused target is available.
        "focus_safe_overlay".into(),
    ];
    if platform_capabilities.focused_target {
        capabilities.push("focused_target".into());
    }
    if platform_capabilities.keyboard_injection {
        capabilities.push("keyboard_insertion".into());
    }
    if platform_capabilities.clipboard {
        capabilities.push("clipboard".into());
    }
    if platform_capabilities.global_shortcut {
        capabilities.push("global_shortcut".into());
    }
    if platform_capabilities.tray {
        capabilities.push("tray".into());
    }
    if platform_capabilities.autostart {
        capabilities.push("autostart".into());
    }
    #[cfg(feature = "native")]
    capabilities.extend([
        "local_whisper".into(),
        "post_processing".into(),
        "prompt_rewrite".into(),
    ]);
    #[cfg(feature = "vulkan")]
    {
        capabilities.push("vulkan_acceleration".into());
        info!("local Whisper GPU acceleration enabled backend=vulkan");
    }
    #[cfg(feature = "metal")]
    {
        capabilities.push("metal_acceleration".into());
        info!("local Whisper GPU acceleration enabled backend=metal");
    }

    let service = SpeakiputService::new(
        settings_repository.clone(),
        history_repository,
        capabilities,
        vec![AudioDevice {
            id: "default".into(),
            name: "Default microphone".into(),
            is_default: true,
        }],
    )
    .with_credentials(credential_repository.clone());
    #[cfg(feature = "native")]
    {
        use native::{ConfiguredLocalWhisper, ConfiguredPostProcessor};
        use speakiput_audio::CpalAudioSource;

        let asr = Arc::new(ConfiguredLocalWhisper::new(settings_repository.clone()));
        let post_processor = Some(Arc::new(ConfiguredPostProcessor::new(
            settings_repository.clone(),
            credential_repository.clone(),
        )) as Arc<dyn speakiput_llm::PostProcessor>);
        let prompt_rewriter = Some(Arc::new(ConfiguredPostProcessor::new(
            settings_repository.clone(),
            credential_repository,
        )) as Arc<dyn speakiput_llm::PromptRewriter>);
        let service = service.with_runtime(RuntimeComponents {
            audio: Arc::new(CpalAudioSource),
            asr,
            post_processor,
            prompt_rewriter,
            focus: platform.clone(),
            output: platform,
        });
        #[cfg(target_os = "linux")]
        let shortcut = Arc::new(LinuxShortcutService::default());
        #[cfg(target_os = "macos")]
        let shortcut = Arc::new(MacShortcutService::default());
        return run(service, settings_repository, shortcut).await;
    }
    #[cfg(not(feature = "native"))]
    {
        #[cfg(target_os = "linux")]
        let shortcut = Arc::new(LinuxShortcutService::default());
        #[cfg(target_os = "macos")]
        let shortcut = Arc::new(MacShortcutService::default());
        return run(service, settings_repository, shortcut).await;
    }
}

async fn toggle_recording() -> Result<(), Box<dyn std::error::Error>> {
    let client = UnixBackendClient::connect(default_socket_path()).await?;
    client
        .request(Envelope::request(
            "client.hello",
            serde_json::to_value(ClientHelloRequest {
                supported_versions: vec![PROTOCOL_VERSION.into()],
                client: ClientIdentity {
                    name: "speakiput-shortcut".into(),
                    version: env!("CARGO_PKG_VERSION").into(),
                },
                subscriptions: vec![],
            })?,
        ))
        .await?;
    let state: StateSnapshot = client
        .request(Envelope::request("state.get", serde_json::json!({})))
        .await?
        .payload_as()?;
    match (state.state, state.active_session_id) {
        (DictationState::Idle, _) => {
            let settings: SettingsResponse = client
                .request(Envelope::request("settings.get", serde_json::json!({})))
                .await?
                .payload_as()?;
            client
                .request(Envelope::request(
                    "recording.start",
                    serde_json::to_value(RecordingStartRequest {
                        language: Some(settings.settings.general.language),
                    })?,
                ))
                .await?;
        }
        (DictationState::Recording, Some(session_id)) => {
            client
                .request(Envelope::request(
                    "recording.stop",
                    serde_json::to_value(RecordingStopRequest { session_id })?,
                ))
                .await?;
        }
        _ => {
            return Err(std::io::Error::other(format!(
                "dictation cannot toggle while state is {:?}",
                state.state
            ))
            .into());
        }
    }
    Ok(())
}

async fn run(
    service: SpeakiputService,
    settings: Arc<dyn SettingsRepository>,
    shortcut: Arc<impl ShortcutService + 'static>,
) -> Result<(), Box<dyn std::error::Error>> {
    let service = Arc::new(service);
    let shortcut_task = tokio::spawn(run_shortcut_loop(Arc::clone(&service), settings, shortcut));
    let backend_service: Arc<dyn BackendService> = service;
    let socket = default_socket_path();
    info!(path = %socket.display(), "speakiputd listening");
    let result = serve_until(socket, backend_service, async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await;
    shortcut_task.abort();
    result.map_err(Into::into)
}

async fn run_shortcut_loop(
    service: Arc<SpeakiputService>,
    settings: Arc<dyn SettingsRepository>,
    shortcut: Arc<impl ShortcutService + 'static>,
) {
    let mut events = service.subscribe().expect("service event channel");
    loop {
        let configured = match settings.get() {
            Ok(stored) => stored.settings.shortcut.record,
            Err(error) => {
                warn!(%error, "cannot load global shortcut setting");
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                continue;
            }
        };
        if let Err(error) = shortcut.register_record_shortcut(&configured).await {
            warn!(%error, shortcut = %configured, "global shortcut is unavailable");
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            continue;
        }
        info!(shortcut = %configured, "global shortcut registered");

        loop {
            tokio::select! {
                activation = shortcut.next_activation() => match activation {
                    Ok(()) => {
                        if let Err(error) = service.activate_record_shortcut().await {
                            warn!(%error, "global shortcut activation failed");
                        }
                    }
                    Err(error) => {
                        warn!(%error, "global shortcut session ended");
                        break;
                    }
                },
                event = events.recv() => match event {
                    Ok(event) if event.name == "settings.changed" => break,
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => break,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                }
            }
        }
        let _ = shortcut.unregister_all().await;
    }
}
