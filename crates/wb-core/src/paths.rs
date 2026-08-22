//! Data location contract (docs/技术方案-v0.1.md §0):
//! settings/db/audit under %APPDATA%\WB, caches/plugins under %LOCALAPPDATA%\WB.

use std::path::PathBuf;

pub fn app_data_dir() -> PathBuf {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("WB")
}

pub fn local_data_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("WB")
}

pub fn db_path() -> PathBuf {
    if let Ok(p) = std::env::var("WB_DB_PATH") {
        return PathBuf::from(p);
    }
    app_data_dir().join("wb.db")
}

pub fn settings_path() -> PathBuf {
    app_data_dir().join("settings.json")
}

/// Named-pipe endpoint shared by daemon and clients.
pub fn pipe_name() -> &'static str {
    "wb-daemon"
}
