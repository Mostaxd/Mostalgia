use std::path::{Path, PathBuf};

pub const WHISPER_MODEL: &str = "ggml-large-v3-turbo.bin";
pub const VAD_MODEL: &str = "ggml-silero-v5.1.2.bin";

/// `$XDG_DATA_HOME/mostalgia` (usually `~/.local/share/mostalgia`): the
/// database, recordings and models live here. `MOSTALGIA_DATA_DIR`
/// overrides it (e.g. for a separate demo library). On Windows it is the
/// local AppData folder, so gigabytes of models and audio don't roam with
/// the profile.
pub fn data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("MOSTALGIA_DATA_DIR") {
        return PathBuf::from(dir);
    }
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("mostalgia")
}

pub fn database() -> PathBuf {
    data_dir().join("mostalgia.db")
}

pub fn recordings_dir() -> PathBuf {
    data_dir().join("recordings")
}

/// Finds a model file: `./models/<name>` (a source checkout) first, then
/// the data directory's `models/`. Returns the data-dir path if neither
/// exists, so error messages point at the canonical location.
pub fn model(name: &str) -> PathBuf {
    let local = Path::new("models").join(name);
    if local.exists() {
        return local;
    }
    data_dir().join("models").join(name)
}

/// Unix socket the desktop app listens on for `mostalgia toggle` etc.
pub fn control_socket() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(dir) => PathBuf::from(dir).join("mostalgia.sock"),
        None => std::env::temp_dir().join(format!(
            "mostalgia-{}.sock",
            std::env::var("USER").unwrap_or_default()
        )),
    }
}
