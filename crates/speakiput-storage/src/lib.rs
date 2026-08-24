// History behavior adapted from whisrs src/history.rs at commit
// 28139bd8c4ff17e8d0fd156a0d903a7baa423d48. Copyright (c) 2025-present
// Yosif Kitaneh, used under the MIT License; see THIRD_PARTY_LICENSES.md.

//! Settings and history repository implementations.

use std::{
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    sync::Mutex,
};

use serde::{Deserialize, Serialize};
use speakiput_contract::{HistoryEntry, Settings};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("storage I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("stored JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("settings validation failed at {field}: {message}")]
    Validation {
        field: &'static str,
        message: &'static str,
    },
    #[error("settings revision conflict: expected {expected}, current {current}")]
    Conflict { expected: u64, current: u64 },
    #[error("repository lock is poisoned")]
    Poisoned,
    #[error("credential validation failed at {field}: {message}")]
    CredentialValidation {
        field: &'static str,
        message: &'static str,
    },
    #[error("secure credential storage failed: {0}")]
    Credential(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RevisionedSettings {
    pub revision: u64,
    pub settings: Settings,
}

pub trait SettingsRepository: Send + Sync {
    fn get(&self) -> Result<RevisionedSettings, StorageError>;
    fn replace(
        &self,
        expected_revision: u64,
        settings: Settings,
    ) -> Result<RevisionedSettings, StorageError>;
}

pub trait HistoryRepository: Send + Sync {
    fn append(&self, entry: &HistoryEntry) -> Result<(), StorageError>;
    fn list(
        &self,
        limit: usize,
        cursor: Option<&str>,
    ) -> Result<(Vec<HistoryEntry>, Option<String>), StorageError>;
    fn clear(&self) -> Result<(), StorageError>;
}

pub trait CredentialRepository: Send + Sync {
    fn put(&self, credential_id: &str, secret: &str) -> Result<(), StorageError>;
    fn get(&self, credential_id: &str) -> Result<Option<String>, StorageError>;
    fn delete(&self, credential_id: &str) -> Result<(), StorageError>;
}

#[derive(Debug, Default)]
pub struct SystemCredentialRepository;

impl SystemCredentialRepository {
    fn entry(credential_id: &str) -> Result<keyring::Entry, StorageError> {
        if credential_id.trim().is_empty() {
            return Err(StorageError::CredentialValidation {
                field: "credential_id",
                message: "must not be empty",
            });
        }
        keyring::Entry::new("speakiput", credential_id)
            .map_err(|error| StorageError::Credential(error.to_string()))
    }
}

impl CredentialRepository for SystemCredentialRepository {
    fn put(&self, credential_id: &str, secret: &str) -> Result<(), StorageError> {
        if secret.is_empty() {
            return Err(StorageError::CredentialValidation {
                field: "secret",
                message: "must not be empty",
            });
        }
        Self::entry(credential_id)?
            .set_password(secret)
            .map_err(|error| StorageError::Credential(error.to_string()))
    }

    fn get(&self, credential_id: &str) -> Result<Option<String>, StorageError> {
        match Self::entry(credential_id)?.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(StorageError::Credential(error.to_string())),
        }
    }

    fn delete(&self, credential_id: &str) -> Result<(), StorageError> {
        match Self::entry(credential_id)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(StorageError::Credential(error.to_string())),
        }
    }
}

#[derive(Debug)]
pub struct JsonSettingsRepository {
    path: PathBuf,
    lock: Mutex<()>,
}

impl JsonSettingsRepository {
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            lock: Mutex::new(()),
        }
    }

    fn load(&self) -> Result<RevisionedSettings, StorageError> {
        if !self.path.exists() {
            return Ok(RevisionedSettings {
                revision: 0,
                settings: Settings::default(),
            });
        }
        let stored = serde_json::from_slice::<RevisionedSettings>(&fs::read(&self.path)?)?;
        validate_settings(&stored.settings)?;
        Ok(stored)
    }
}

impl SettingsRepository for JsonSettingsRepository {
    fn get(&self) -> Result<RevisionedSettings, StorageError> {
        let _guard = self.lock.lock().map_err(|_| StorageError::Poisoned)?;
        self.load()
    }

    fn replace(
        &self,
        expected_revision: u64,
        settings: Settings,
    ) -> Result<RevisionedSettings, StorageError> {
        validate_settings(&settings)?;
        let _guard = self.lock.lock().map_err(|_| StorageError::Poisoned)?;
        let current = self.load()?;
        if current.revision != expected_revision {
            return Err(StorageError::Conflict {
                expected: expected_revision,
                current: current.revision,
            });
        }
        let updated = RevisionedSettings {
            revision: current.revision.saturating_add(1),
            settings,
        };
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temporary = self.path.with_extension("json.tmp");
        fs::write(&temporary, serde_json::to_vec_pretty(&updated)?)?;
        fs::rename(temporary, &self.path)?;
        Ok(updated)
    }
}

fn validate_settings(settings: &Settings) -> Result<(), StorageError> {
    settings
        .validate()
        .map_err(|error| StorageError::Validation {
            field: error.field,
            message: error.message,
        })
}

#[derive(Debug)]
pub struct JsonlHistoryRepository {
    path: PathBuf,
    lock: Mutex<()>,
}

impl JsonlHistoryRepository {
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            lock: Mutex::new(()),
        }
    }

    fn all_entries(&self) -> Result<Vec<HistoryEntry>, StorageError> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let reader = BufReader::new(fs::File::open(&self.path)?);
        Ok(reader
            .lines()
            .map_while(Result::ok)
            .filter(|line| !line.trim().is_empty())
            .filter_map(|line| serde_json::from_str(&line).ok())
            .collect())
    }
}

impl HistoryRepository for JsonlHistoryRepository {
    fn append(&self, entry: &HistoryEntry) -> Result<(), StorageError> {
        let _guard = self.lock.lock().map_err(|_| StorageError::Poisoned)?;
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        serde_json::to_writer(&mut file, entry)?;
        writeln!(file)?;
        Ok(())
    }

    fn list(
        &self,
        limit: usize,
        cursor: Option<&str>,
    ) -> Result<(Vec<HistoryEntry>, Option<String>), StorageError> {
        let _guard = self.lock.lock().map_err(|_| StorageError::Poisoned)?;
        let offset = cursor
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(0);
        let mut entries = self.all_entries()?;
        entries.reverse();
        let page = entries
            .iter()
            .skip(offset)
            .take(limit)
            .cloned()
            .collect::<Vec<_>>();
        let next_offset = offset.saturating_add(page.len());
        let next = (next_offset < entries.len()).then(|| next_offset.to_string());
        Ok((page, next))
    }

    fn clear(&self) -> Result<(), StorageError> {
        let _guard = self.lock.lock().map_err(|_| StorageError::Poisoned)?;
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

#[must_use]
pub fn default_paths(data_dir: &Path, config_dir: &Path) -> (PathBuf, PathBuf) {
    (
        config_dir.join("speakiput/settings.json"),
        data_dir.join("speakiput/history.jsonl"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn entry(index: usize) -> HistoryEntry {
        HistoryEntry {
            session_id: Uuid::new_v4(),
            raw_text: format!("raw {index}"),
            processed_text: format!("processed {index}"),
            output_text: format!("output {index}"),
            created_at: format!("2026-08-23T21:30:0{index}Z"),
        }
    }

    #[test]
    fn settings_replace_is_atomic_and_revision_checked() {
        let directory = tempfile::tempdir().unwrap();
        let repository = JsonSettingsRepository::new(directory.path().join("settings.json"));
        assert_eq!(repository.get().unwrap().revision, 0);
        let updated = repository.replace(0, Settings::default()).unwrap();
        assert_eq!(updated.revision, 1);
        assert!(matches!(
            repository.replace(0, Settings::default()),
            Err(StorageError::Conflict { current: 1, .. })
        ));
        assert_eq!(repository.get().unwrap(), updated);
    }

    #[test]
    fn invalid_settings_are_not_written() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let repository = JsonSettingsRepository::new(&path);
        let mut settings = Settings::default();
        settings.general.auto_stop_ms = 0;
        assert!(matches!(
            repository.replace(0, settings),
            Err(StorageError::Validation { .. })
        ));
        assert!(!path.exists());
    }

    #[test]
    fn history_is_newest_first_paginated_and_clearable() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("history.jsonl");
        let repository = JsonlHistoryRepository::new(&path);
        for index in 0..5 {
            repository.append(&entry(index)).unwrap();
        }
        let (first, cursor) = repository.list(3, None).unwrap();
        assert_eq!(first[0].raw_text, "raw 4");
        let (second, cursor) = repository.list(3, cursor.as_deref()).unwrap();
        assert_eq!(second.len(), 2);
        assert!(cursor.is_none());
        repository.clear().unwrap();
        assert!(repository.list(10, None).unwrap().0.is_empty());
    }

    #[test]
    fn malformed_history_lines_are_skipped() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("history.jsonl");
        fs::write(
            &path,
            format!("bad json\n{}\n", serde_json::to_string(&entry(1)).unwrap()),
        )
        .unwrap();
        assert_eq!(
            JsonlHistoryRepository::new(path)
                .list(10, None)
                .unwrap()
                .0
                .len(),
            1
        );
    }
}
