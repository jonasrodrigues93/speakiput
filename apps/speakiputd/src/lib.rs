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
    #[cfg(target_os = "macos")]
    {
        let application_support = std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|home| home.join("Library/Application Support"))
            .unwrap_or_else(std::env::temp_dir);
        return speakiput_storage::default_paths(&application_support, &application_support);
    }
    #[cfg(not(target_os = "macos"))]
    {
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
            .unwrap_or_else(std::env::temp_dir);
        let data = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share"))
            })
            .unwrap_or_else(std::env::temp_dir);
        speakiput_storage::default_paths(&data, &config)
    }
}
