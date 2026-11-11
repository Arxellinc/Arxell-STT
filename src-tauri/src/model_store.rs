//! One pinned model; partial downloads never replace a verified installation.

use crate::engine::{DEFAULT_MODEL_SHA256, DEFAULT_MODEL_SIZE, verify_model};
use serde::Serialize;
use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::Duration,
};
use tauri::{AppHandle, Emitter, Manager};

pub const MODEL_NAME: &str = "Parakeet Unified · English · Q4_K_M";
pub const FILE_NAME: &str = "parakeet-unified-en-0.6b-Q4_K_M.gguf";
const URL: &str = "https://huggingface.co/handy-computer/parakeet-unified-en-0.6b-gguf/resolve/843b1b175ace04f5a89d379c79e2518c0d065b25/parakeet-unified-en-0.6b-Q4_K_M.gguf";

pub struct Download {
    pub cancel: Arc<AtomicBool>,
    pub thread: JoinHandle<()>,
}

#[derive(Clone, Serialize)]
pub struct DownloadEvent {
    pub received: u64,
    pub total: u64,
    pub state: String,
    pub message: String,
}

pub fn default_path(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("models")
        .join(FILE_NAME))
}

pub fn verified(path: &Path) -> Result<(), String> {
    verify_model(path, DEFAULT_MODEL_SHA256, Some(DEFAULT_MODEL_SIZE))
}

pub fn download(app: AppHandle, cancel: Arc<AtomicBool>) {
    let progress = |received, state: &str, message: String| {
        let _ = app.emit(
            "model-download",
            DownloadEvent {
                received,
                total: DEFAULT_MODEL_SIZE,
                state: state.into(),
                message,
            },
        );
    };
    let transfer = async {
        let destination = default_path(&app)?;
        if verified(&destination).is_ok() {
            return Ok(destination);
        }
        std::fs::create_dir_all(destination.parent().ok_or("model directory missing")?)
            .map_err(|e| e.to_string())?;
        let partial = destination.with_extension("gguf.part");
        let mut offset = std::fs::metadata(&partial).map_or(0, |m| m.len());
        if offset > DEFAULT_MODEL_SIZE {
            File::create(&partial).map_err(|e| e.to_string())?;
            offset = 0;
        }
        if offset < DEFAULT_MODEL_SIZE {
            // Immediate feedback for UI (and webview reloads): the first chunk
            // event otherwise arrives up to 250 ms later.
            progress(offset, "downloading", String::new());
            let client = reqwest::Client::builder()
                .https_only(true)
                .connect_timeout(Duration::from_secs(15))
                .read_timeout(Duration::from_secs(15))
                .timeout(Duration::from_secs(1800))
                .build()
                .map_err(|e| e.to_string())?;
            let mut request = client.get(URL);
            if offset > 0 {
                request = request.header(reqwest::header::RANGE, format!("bytes={offset}-"));
            }
            let mut response = request
                .send()
                .await
                .and_then(|r| r.error_for_status())
                .map_err(|e| e.to_string())?;
            if response.status() == reqwest::StatusCode::PARTIAL_CONTENT {
                let expected = format!("bytes {offset}-");
                if !response
                    .headers()
                    .get(reqwest::header::CONTENT_RANGE)
                    .and_then(|v| v.to_str().ok())
                    .is_some_and(|s| s.starts_with(&expected))
                {
                    return Err("Model server returned an invalid resume range.".into());
                }
            } else {
                offset = 0;
            }
            let mut file = OpenOptions::new()
                .create(true)
                .write(true)
                .append(offset > 0)
                .truncate(offset == 0)
                .open(&partial)
                .map_err(|e| e.to_string())?;
            let mut last_progress = std::time::Instant::now();
            loop {
                if cancel.load(Ordering::Relaxed) {
                    return Err("Download cancelled. Partial data kept for resume.".into());
                }
                let Some(bytes) = response.chunk().await.map_err(|e| e.to_string())? else {
                    break;
                };
                offset += bytes.len() as u64;
                if offset > DEFAULT_MODEL_SIZE {
                    return Err("Model download exceeded its expected size.".into());
                }
                file.write_all(&bytes).map_err(|e| e.to_string())?;
                if last_progress.elapsed() >= Duration::from_millis(250) {
                    progress(offset, "downloading", String::new());
                    last_progress = std::time::Instant::now();
                }
            }
            file.sync_all().map_err(|e| e.to_string())?;
        }
        if cancel.load(Ordering::Relaxed) {
            return Err("Download cancelled.".into());
        }
        progress(offset, "verifying", String::new());
        if let Err(error) = verified(&partial) {
            // An invalid complete partial cannot be resumed meaningfully.
            std::fs::rename(&partial, partial.with_extension("invalid"))
                .map_err(|e| e.to_string())?;
            return Err(error);
        }
        std::fs::rename(&partial, &destination).map_err(|e| e.to_string())?;
        Ok::<_, String>(destination)
    };
    let result = tauri::async_runtime::block_on(async {
        tokio::select! {
            result = transfer => result,
            _ = async { while !cancel.load(Ordering::Relaxed) { tokio::time::sleep(Duration::from_millis(100)).await; } } => Err("Download cancelled. Partial data kept for resume.".into()),
        }
    });
    match result {
        Ok(path) => progress(
            DEFAULT_MODEL_SIZE,
            "complete",
            path.to_string_lossy().into_owned(),
        ),
        Err(message) => progress(
            0,
            if cancel.load(Ordering::Relaxed) {
                "cancelled"
            } else {
                "error"
            },
            message,
        ),
    }
}

/// Start the curated-model download exactly once, whether the trigger was
/// the first-run auto-fetch or the Settings button. Reuses the shared
/// per-process guard so two triggers cannot spawn two writers.
pub fn start(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<crate::app::AppState>();
    let mut slot = state
        .download
        .lock()
        .map_err(|_| "download lock poisoned")?;
    if slot.as_ref().is_some_and(|d| !d.thread.is_finished()) {
        return Err("model download already running".into());
    }
    let cancel = Arc::new(AtomicBool::new(false));
    let worker_cancel = cancel.clone();
    let handle = app.clone();
    let thread = std::thread::Builder::new()
        .name("model-download".into())
        .spawn(move || download(handle, worker_cancel))
        .map_err(|e| e.to_string())?;
    *slot = Some(Download { cancel, thread });
    Ok(())
}

/// Auto-fetch applies only while no custom model location was chosen: the
/// configured path must be empty (nothing selected yet) or the curated
/// default itself. Any other value is a user decision we never override.
fn should_auto_fetch(configured: &str, default: &Path) -> bool {
    let trimmed = configured.trim();
    trimmed.is_empty() || Path::new(trimmed) == default
}

/// First-run convenience: when the configured model is the curated default
/// (or unset) and no verified copy exists yet, fetch it automatically so the
/// app becomes usable without visiting Settings. Errors surface through the
/// normal download events; they never block startup.
pub fn auto_fetch(app: &AppHandle) -> Result<bool, String> {
    let default = default_path(app)?;
    let configured = {
        let state = app.state::<crate::app::AppState>();
        state
            .settings
            .lock()
            .map_err(|_| "settings lock poisoned")?
            .model_path
            .clone()
    };
    if !should_auto_fetch(&configured, &default) {
        return Ok(false);
    }
    if verified(&default).is_ok() {
        return Ok(false);
    }
    start(app)?;
    crate::app::trace(format_args!(
        "model auto-fetch started: curated model missing at {}",
        default.display()
    ));
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::should_auto_fetch;
    use std::path::Path;

    #[test]
    fn auto_fetch_targets_only_the_default_location() {
        let default = Path::new("/app-data/models/parakeet-unified-en-0.6b-Q4_K_M.gguf");
        assert!(
            should_auto_fetch("", default),
            "unset model must auto-fetch"
        );
        assert!(should_auto_fetch("   ", default), "blank is unset");
        assert!(
            should_auto_fetch(default.to_str().unwrap(), default),
            "the curated default path itself must auto-fetch"
        );
    }

    #[test]
    fn auto_fetch_never_overrides_a_custom_model_choice() {
        let default = Path::new("/app-data/models/parakeet-unified-en-0.6b-Q4_K_M.gguf");
        assert!(!should_auto_fetch("/mnt/big/model.gguf", default));
        assert!(!should_auto_fetch(
            "/app-data/models/some-other-model.gguf",
            default
        ));
    }
}
