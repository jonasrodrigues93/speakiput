//! Background service and Unix IPC transport for speakiput.

pub mod server;
pub mod service;

use std::path::PathBuf;

#[must_use]
pub fn default_socket_path() -> PathBuf {
    speakiput_client::default_socket_path()
}

#[must_use]
pub fn default_storage_paths() -> (PathBuf, PathBuf) {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .unwrap_or_else(std::env::temp_dir);
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .unwrap_or_else(std::env::temp_dir);
    speakiput_storage::default_paths(&data, &config)
}
