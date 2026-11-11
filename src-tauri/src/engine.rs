//! transcribe.cpp adapter and backend policy.
//!
//! Auto tries the platform GPU first, then falls back to CPU exactly once.
//! The actual bound backend/device is reported, never inferred.

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;
use std::sync::OnceLock;

use serde::Serialize;
use sha2::{Digest, Sha256};
use transcribe_cpp::{
    Backend, CommitPolicy, Model, ModelOptions, ParakeetBufferedStreamOptions, RunOptions,
    StreamExtension, StreamOptions, backend_available, devices, init_backends_default,
};

use crate::capture::AudioFrame;

pub const DEFAULT_MODEL_SHA256: &str =
    "a8bf3de2b393bd14ead5a858c3748d5e3b07a20fdeabdd3b498fba4f463fa929";
pub const DEFAULT_MODEL_SIZE: u64 = 477_274_496;
pub const DEFAULT_MODEL_PATH: &str = "models/parakeet-unified-en-0.6b-Q4_K_M.gguf";

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
pub enum BackendPreference {
    #[default]
    Auto,
    CpuOnly,
    GpuOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    Cpu,
    Vulkan,
    Metal,
    Cuda,
}

#[derive(Debug, Clone, Serialize)]
pub struct BackendAvailability {
    pub name: String,
    pub available: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeviceInfo {
    pub name: String,
    pub kind: String,
    pub description: String,
    pub memory_total: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProbeStatus {
    pub backends: Vec<BackendAvailability>,
    pub devices: Vec<DeviceInfo>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EngineInfo {
    pub backend: String,
    pub device: String,
}

static INIT: OnceLock<Result<(), String>> = OnceLock::new();

fn ensure_backends() -> Result<(), String> {
    INIT.get_or_init(|| init_backends_default().map_err(|error| error.to_string()))
        .clone()
}

fn preferred_gpu() -> Option<Backend> {
    #[cfg(target_os = "linux")]
    {
        backend_available(Backend::Vulkan).then_some(Backend::Vulkan)
    }
    #[cfg(target_os = "macos")]
    {
        backend_available(Backend::Metal).then_some(Backend::Metal)
    }
    #[cfg(target_os = "windows")]
    {
        if backend_available(Backend::Vulkan) {
            Some(Backend::Vulkan)
        } else if backend_available(Backend::Cuda) {
            Some(Backend::Cuda)
        } else {
            None
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        None
    }
}

pub fn probe_status() -> Result<ProbeStatus, String> {
    ensure_backends()?;
    let devices = devices()
        .into_iter()
        .map(|device| DeviceInfo {
            name: device.name,
            kind: device.kind,
            description: device.description,
            memory_total: device.memory_total,
        })
        .collect();
    let backends = [
        Backend::Cpu,
        Backend::Metal,
        Backend::Vulkan,
        Backend::Cuda,
        Backend::Rocm,
    ]
    .into_iter()
    .map(|backend| BackendAvailability {
        name: format!("{backend:?}"),
        available: backend_available(backend),
    })
    .collect();
    Ok(ProbeStatus { backends, devices })
}

pub fn sha256_file(path: &Path) -> Result<String, String> {
    let file = File::open(path).map_err(|error| format!("open {}: {error}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 1024 * 1024];

    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| format!("read {}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }

    Ok(format!("{:x}", hasher.finalize()))
}

pub fn verify_model(
    path: &Path,
    expected_sha256: &str,
    expected_size: Option<u64>,
) -> Result<(), String> {
    let metadata =
        std::fs::metadata(path).map_err(|error| format!("model {}: {error}", path.display()))?;

    if let Some(expected_size) = expected_size
        && metadata.len() != expected_size
    {
        return Err(format!(
            "model size mismatch: expected {expected_size} bytes, got {}",
            metadata.len()
        ));
    }

    let actual = sha256_file(path)?;
    if !actual.eq_ignore_ascii_case(expected_sha256) {
        return Err(format!(
            "model hash mismatch: expected {expected_sha256}, got {actual}"
        ));
    }
    Ok(())
}

/// Buffered Parakeet streaming geometry. Cost scales with (70 + C + R) / C,
/// so smaller chunks are far more expensive; see `docs/review/runtime-results.md`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StreamPreset {
    /// (70,13,2): 1.2 s lookahead, ~0.21 RTF measured on this GPU.
    #[default]
    Efficient,
    /// (70,7,2): 0.72 s lookahead, ~0.36 RTF.
    Responsive,
    /// (70,13,13): 2.08 s lookahead, ~0.23 RTF.
    Accurate,
}

impl StreamPreset {
    /// Keep long-session efficiency, but do not make the first phrase pay the
    /// largest efficient chunk's startup buffering. Accurate remains opt-in.
    pub fn interactive_startup(self) -> Self {
        match self {
            Self::Efficient => Self::Responsive,
            other => other,
        }
    }

    pub fn chunk_ms(self) -> i64 {
        match self {
            Self::Efficient | Self::Accurate => 1_040,
            Self::Responsive => 560,
        }
    }

    pub fn right_ms(self) -> i64 {
        match self {
            Self::Efficient | Self::Responsive => 160,
            Self::Accurate => 1_040,
        }
    }

    /// Audio that must be buffered before a chunk's tokens emerge.
    pub fn lookahead_ms(self) -> u64 {
        (self.chunk_ms() + self.right_ms()) as u64
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Efficient => "Efficient",
            Self::Responsive => "Responsive",
            Self::Accurate => "Accurate",
        }
    }
}

pub trait StreamHooks {
    fn next_frame(&mut self) -> Option<AudioFrame>;
    fn on_text(&mut self, input_ms: i64, committed: &str, tentative: &str);
    fn cancelled(&self) -> bool {
        false
    }
}

#[derive(Debug, Clone)]
pub struct StreamOutcome {
    pub final_text: String,
    pub final_committed: String,
    pub committed_mutations: usize,
    pub divergent: bool,
    /// Longest single `feed` call; indicates chunk-boundary stalls.
    pub max_feed_ms: u64,
    /// Audio actually fed to the engine.
    pub fed_ms: u64,
}

pub struct Engine {
    model: Model,
    backend: String,
    device: String,
}

impl Engine {
    pub fn load_verified(
        path: &Path,
        expected_sha256: &str,
        expected_size: Option<u64>,
        preference: BackendPreference,
    ) -> Result<Self, String> {
        verify_model(path, expected_sha256, expected_size)?;
        Self::load(path, preference)
    }

    pub fn load(path: &Path, preference: BackendPreference) -> Result<Self, String> {
        ensure_backends()?;
        let mut attempts = Vec::new();

        if preference != BackendPreference::CpuOnly {
            if let Some(gpu) = preferred_gpu() {
                match Model::load_with(
                    path,
                    &ModelOptions {
                        backend: gpu,
                        ..Default::default()
                    },
                ) {
                    Ok(model) => return Self::from_model(model),
                    Err(error) => attempts.push(format!("{gpu:?}: {error}")),
                }
            } else {
                attempts.push("GPU: no usable device".to_string());
            }
        }

        if preference == BackendPreference::GpuOnly {
            return Err(format!(
                "GPU-only requested but load failed: {}",
                attempts.join("; ")
            ));
        }

        Model::load_with(
            path,
            &ModelOptions {
                backend: Backend::Cpu,
                ..Default::default()
            },
        )
        .map_err(|error| {
            format!(
                "CPU load failed: {error}; GPU attempts: {}",
                attempts.join("; ")
            )
        })
        .and_then(Self::from_model)
    }

    fn from_model(model: Model) -> Result<Self, String> {
        let backend = model.backend();
        let device = model
            .device()
            .map(|device| format!("{} ({})", device.name, device.kind))
            .unwrap_or_else(|_| "unknown".to_string());
        Ok(Self {
            model,
            backend,
            device,
        })
    }

    pub fn info(&self) -> EngineInfo {
        EngineInfo {
            backend: self.backend.clone(),
            device: self.device.clone(),
        }
    }

    pub fn transcribe(&mut self, pcm: &[f32]) -> Result<String, String> {
        self.transcribe_cancellable(pcm, &transcribe_cpp::CancelToken::new())
    }

    pub fn transcribe_cancellable(
        &mut self,
        pcm: &[f32],
        cancel: &transcribe_cpp::CancelToken,
    ) -> Result<String, String> {
        let mut session = self.model.session().map_err(|error| error.to_string())?;
        session.set_cancel_token(cancel);
        let result = session
            .run(pcm, &RunOptions::default())
            .map_err(|error| error.to_string())?;
        Ok(result.text)
    }

    pub fn run_stream<H: StreamHooks>(
        &mut self,
        hooks: &mut H,
        preset: StreamPreset,
    ) -> Result<StreamOutcome, String> {
        self.run_stream_cancellable(hooks, preset, &transcribe_cpp::CancelToken::new())
    }

    pub fn run_stream_cancellable<H: StreamHooks>(
        &mut self,
        hooks: &mut H,
        preset: StreamPreset,
        cancel: &transcribe_cpp::CancelToken,
    ) -> Result<StreamOutcome, String> {
        let options = StreamOptions {
            commit_policy: CommitPolicy::Auto,
            family: Some(StreamExtension::ParakeetBuffered(
                ParakeetBufferedStreamOptions {
                    left_ms: Some(5_600),
                    chunk_ms: Some(preset.chunk_ms() as i32),
                    right_ms: Some(preset.right_ms() as i32),
                },
            )),
            ..Default::default()
        };

        let mut session = self.model.session().map_err(|error| error.to_string())?;
        session.set_cancel_token(cancel);
        let mut stream = session
            .stream(&RunOptions::default(), &options)
            .map_err(|error| error.to_string())?;

        let mut committed = String::new();
        let mut committed_mutations = 0usize;
        let mut max_feed = std::time::Duration::ZERO;
        let mut fed_samples = 0u64;

        while let Some(frame) = hooks.next_frame() {
            if hooks.cancelled() || cancel.is_cancelled() {
                return Err("cancelled".into());
            }
            let started = std::time::Instant::now();
            let update = stream.feed(&frame.pcm).map_err(|error| error.to_string())?;
            max_feed = max_feed.max(started.elapsed());
            fed_samples += frame.pcm.len() as u64;
            if update.committed_changed || update.tentative_changed {
                let text = stream.text();
                if text.committed.len() + text.tentative.len() > 2 * 1024 * 1024 {
                    return Err("Transcript exceeded the session text limit.".into());
                }
                if !text.committed.starts_with(&committed) {
                    committed_mutations += 1;
                }
                committed = text.committed.clone();
                hooks.on_text(update.input_received_ms, &text.committed, &text.tentative);
            }
        }

        if hooks.cancelled() || cancel.is_cancelled() {
            return Err("cancelled".into());
        }
        stream.finalize().map_err(|error| error.to_string())?;
        let text = stream.text();
        if !text.committed.starts_with(&committed) {
            committed_mutations += 1;
        }
        let divergent = !text.full.starts_with(&text.committed);

        Ok(StreamOutcome {
            final_text: text.full,
            final_committed: text.committed,
            committed_mutations,
            divergent,
            max_feed_ms: max_feed.as_millis() as u64,
            fed_ms: fed_samples * 1_000 / 16_000,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interactive_startup_reduces_buffering_without_overriding_accuracy() {
        assert!(
            StreamPreset::Efficient.interactive_startup().lookahead_ms()
                < StreamPreset::Efficient.lookahead_ms()
        );
        assert_eq!(
            StreamPreset::Accurate.interactive_startup(),
            StreamPreset::Accurate
        );
    }

    #[test]
    fn hashes_and_verifies_a_file() {
        let path = std::env::temp_dir().join(format!(
            "arxell-sha-{}-{}.txt",
            std::process::id(),
            // Test thread names contain `::`, which is invalid in Windows filenames.
            getrandom::u64().expect("random temp file suffix")
        ));
        std::fs::write(&path, b"abc").expect("write temp file");

        let expected = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert_eq!(sha256_file(&path).expect("hash"), expected);
        assert!(verify_model(&path, expected, Some(3)).is_ok());
        assert!(verify_model(&path, expected, Some(4)).is_err());
        assert!(verify_model(&path, "00", Some(3)).is_err());

        std::fs::remove_file(&path).ok();
    }
}
