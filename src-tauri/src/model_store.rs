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
