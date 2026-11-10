use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use tauri::{AppHandle, Emitter, Manager};

pub const APP_NAME: &str = "Arxell Speak";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const BUILD_ID: &str = "xtest-x11-4";

/// Opt-in, process-local diagnostic events. Callers must only pass metadata,
/// never transcript content, credentials, audio, or destination field values.
pub fn trace(metadata: std::fmt::Arguments<'_>) {
    if std::env::var_os("ARXELL_TRACE").as_deref() == Some(std::ffi::OsStr::new("1")) {
        static START: std::sync::LazyLock<std::time::Instant> =
            std::sync::LazyLock::new(std::time::Instant::now);
        eprintln!(
            "arxell trace: t_ms={} {metadata}",
            START.elapsed().as_millis()
        );
    }
}

/// Coarse model availability shown in the header. "Ready" must mean loaded.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum ModelStatus {
    Loading,
    #[default]
    Missing,
    Ready,
    Error,
}

impl ModelStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Loading => "loading",
            Self::Ready => "ready",
            Self::Missing => "missing",
            Self::Error => "error",
        }
    }
}

/// Single writer for the loaded-model name and status, plus the UI event.
pub fn set_model_status(app: &AppHandle, status: ModelStatus, name: Option<&str>) {
    trace(format_args!("model state={}", status.as_str()));
    let state = app.state::<AppState>();
    if let Ok(mut loaded) = state.loaded_model.lock() {
        *loaded = name.map(str::to_string);
    }
    if let Ok(mut current) = state.model_status.lock() {
        *current = status;
    }
    let _ = app.emit(
        "model-state",
        serde_json::json!({"name": name, "state": status.as_str()}),
    );
}

pub struct SessionHandle {
    pub stop: Arc<AtomicBool>,
    pub cancel: Arc<AtomicBool>,
    pub abort: transcribe_cpp::CancelToken,
    pub running: Arc<AtomicBool>,
    pub thread: JoinHandle<()>,
}

#[derive(Default)]
pub struct AppState {
    pub session: Mutex<Option<SessionHandle>>,
    pub settings: Mutex<crate::settings::Settings>,
    pub next_session: AtomicU64,
    pub engine: Arc<Mutex<ModelCache>>,
    pub api: Mutex<Option<crate::host_api::ApiServer>>,
    pub download: Mutex<Option<crate::model_store::Download>>,
    pub exiting: AtomicBool,
    pub setup_error: Mutex<Option<String>>,
    pub loaded_model: Mutex<Option<String>>,
    pub model_status: Mutex<ModelStatus>,
    pub model_loading: AtomicBool,
    pub interactive_wanted: AtomicBool,
    pub shortcut_held: AtomicBool,
    pub shortcut_capture: AtomicBool,
    pub shortcut_ready: AtomicBool,
    pub setup_busy: AtomicBool,
    pub setup_checks: Mutex<crate::setup::Checks>,
    pub shortcut_generation: AtomicU64,
    pub snapshot: Mutex<SessionSnapshot>,
    /// Serialize revision assignment AND emission across recognition/delivery.
    pub session_events: Mutex<()>,
}

#[derive(Default, Clone, serde::Serialize)]
pub struct SessionSnapshot {
    pub session_id: u64,
    pub revision: u64,
    pub state: String,
    pub text: String,
    pub backend: String,
    #[serde(skip)]
    pub started_at: Option<std::time::Instant>,
    #[serde(skip)]
    pub first_text_emitted: bool,
}

#[derive(Default)]
pub struct ModelCache {
    pub engine: Option<crate::engine::Engine>,
    pub path: String,
    pub backend: crate::engine::BackendPreference,
    pub last_used: Option<std::time::Instant>,
}
