use std::{fs, path::PathBuf};

use serde::Deserialize;
use speakiput_contract::{OutputMode, OverlaySize, Settings};
use speakiput_storage::{
    CredentialRepository, JsonSettingsRepository, SettingsRepository, SystemCredentialRepository,
};

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct WhisrsConfig {
    general: WhisrsGeneral,
    audio: WhisrsAudio,
    input: WhisrsInput,
    #[serde(rename = "local-whisper")]
    local_whisper: WhisrsLocalWhisper,
    llm: WhisrsLlm,
    overlay: WhisrsOverlay,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct WhisrsGeneral {
    language: Option<String>,
    silence_timeout_ms: Option<u64>,
    remove_filler_words: Option<bool>,
    filler_words: Option<Vec<String>>,
    vocabulary: Option<Vec<String>>,
    overlay: Option<bool>,
    llm_post_process: Option<bool>,
    llm_instruction: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct WhisrsAudio {
    device: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct WhisrsInput {
    key_delay_ms: Option<u64>,
    clipboard_only: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct WhisrsLocalWhisper {
    model_path: Option<String>,
    phrase_silence_ms: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct WhisrsLlm {
    api_key: Option<String>,
    model: Option<String>,
    api_url: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct WhisrsOverlay {
    width: Option<u64>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or("HOME is unavailable")?;
    let source = std::env::args_os()
        .nth(1)
        .map_or_else(|| home.join(".config/whisrs/config.toml"), PathBuf::from);
    let config: WhisrsConfig = toml::from_str(&fs::read_to_string(&source)?)?;
    let (settings_path, _) = speakiputd::default_storage_paths();
    let repository = JsonSettingsRepository::new(settings_path);
    let stored = repository.get()?;
    let mut settings = stored.settings;
    apply_config(&mut settings, &config);

    let credential_migrated = config
        .llm
        .api_key
        .as_deref()
        .filter(|secret| !secret.is_empty())
        .is_some_and(|secret| SystemCredentialRepository.put("whisrs-llm", secret).is_ok());
    if credential_migrated {
        settings.post_processing.credential_id = Some("whisrs-llm".into());
    }

    let updated = repository.replace(stored.revision, settings)?;
    println!(
        "Migrated whisrs settings to speakiput revision {} (credential: {})",
        updated.revision,
        if credential_migrated {
            "stored securely"
        } else {
            "not present"
        }
    );
    Ok(())
}

fn apply_config(settings: &mut Settings, config: &WhisrsConfig) {
    if let Some(value) = &config.general.language {
        settings.general.language.clone_from(value);
    }
    if let Some(value) = config.general.silence_timeout_ms {
        settings.general.auto_stop_ms = value;
    }
    if let Some(value) = &config.audio.device {
        settings.audio.input_device_id.clone_from(value);
    }
    if let Some(value) = config.local_whisper.phrase_silence_ms {
        settings.audio.phrase_silence_ms = value;
    }
    if let Some(value) = &config.local_whisper.model_path {
        settings.transcription.model_path = Some(value.clone());
        if let Some(file_name) = PathBuf::from(value)
            .file_stem()
            .and_then(|name| name.to_str())
        {
            settings.transcription.model_id = file_name.trim_start_matches("ggml-").into();
        }
    }
    if let Some(value) = &config.general.vocabulary {
        settings.transcription.vocabulary.clone_from(value);
    }
    if let Some(value) = config.general.remove_filler_words {
        settings.transcription.remove_filler_words = value;
    }
    if let Some(value) = &config.general.filler_words {
        settings.transcription.filler_words.clone_from(value);
    }
    if let Some(value) = config.general.llm_post_process {
        settings.post_processing.enabled = value;
    }
    if let Some(value) = &config.llm.model {
        settings.post_processing.model_id.clone_from(value);
    }
    if let Some(value) = &config.llm.api_url {
        settings.post_processing.endpoint.clone_from(value);
    }
    if let Some(value) = &config.general.llm_instruction {
        settings.post_processing.instruction.clone_from(value);
    }
    if let Some(value) = config.input.key_delay_ms {
        settings.output.key_delay_ms = value;
    }
    if config.input.clipboard_only == Some(true) {
        settings.output.mode = OutputMode::Clipboard;
    }
    if let Some(value) = config.general.overlay {
        settings.overlay.enabled = value;
    }
    settings.overlay.size = match config.overlay.width {
        Some(0..=120) => OverlaySize::Small,
        Some(121..=180) | None => OverlaySize::Medium,
        Some(_) => OverlaySize::Large,
    };
}
