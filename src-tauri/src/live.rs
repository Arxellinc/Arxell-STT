//! Worker-owned dictation.
//!
//! Two modes:
//! - VAD-gated (default): stream each utterance as speech arrives, then
//!   finalize at a pause. Idle silence costs no decode compute.
//! - Continuous: one stream for the whole session (used when gating is off).
//!
//! Stable text is published to an independent desktop delivery worker.
//! Tentative text remains in-app; no desktop Enter is sent.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Instant;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use crate::capture::AudioFrame;
use crate::cue::CueAction;
use crate::desktop::DesktopStream;
use crate::engine::{DEFAULT_MODEL_SHA256, DEFAULT_MODEL_SIZE, Engine, StreamHooks, StreamPreset};
use crate::gate::CueGate;
use crate::mic::Microphone;
#[cfg(test)]
use crate::segments::{Segment, SegmentCollector};
use crate::settings::Settings;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionEvent {
    State {
        state: String,
    },
    Backend {
        backend: String,
        device: String,
    },
    Interim {
        text: String,
    },
    Final {
        text: String,
        action: Option<String>,
    },
    Delivery {
        state: String,
        message: String,
    },
    Warning {
        message: String,
    },
    Error {
        message: String,
    },
}

struct RunResult {
    text: String,
    cue: Option<CueAction>,
    stable: bool,
    capture_error: Option<String>,
    segments: usize,
    fed_ms: u64,
    max_feed_ms: u64,
}

impl Default for RunResult {
    fn default() -> Self {
        Self {
            text: String::new(),
            cue: None,
            stable: true,
            capture_error: None,
            segments: 0,
            fed_ms: 0,
            max_feed_ms: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Continuous mode
// ---------------------------------------------------------------------------

struct ContinuousHooks<'a> {
    microphone: Option<Microphone>,
    gate: CueGate,
    last_speech_ms: u64,
    inactivity_ms: u64,
    stop: Arc<AtomicBool>,
    cancel: Arc<AtomicBool>,
    app: AppHandle,
    last_emitted: String,
    capture_error: Option<String>,
    cues_enabled: bool,
    max_ms: u64,
    recent_audio: VecDeque<AudioFrame>,
    recent_ms: u64,
    desktop: Option<&'a DesktopStream>,
    cue_config: crate::cue::CueConfig,
}

impl StreamHooks for ContinuousHooks<'_> {
    fn next_frame(&mut self) -> Option<AudioFrame> {
        if self.cancel.load(Ordering::Relaxed) || self.gate.action().is_some() {
            return None;
        }
        let microphone = self.microphone.as_mut()?;
        let frame = match microphone.read_frame(&self.stop) {
            Ok(Some(frame)) => frame,
            Ok(None) => return None,
            Err(message) => {
                self.capture_error = Some(message);
                return None;
            }
        };
        if frame.speech {
            self.last_speech_ms = frame.t_end_ms;
        }
        self.recent_audio.push_back(frame.clone());
        while self
            .recent_audio
            .front()
            .is_some_and(|first| frame.t_end_ms.saturating_sub(first.t_start_ms) > self.recent_ms)
        {
            self.recent_audio.pop_front();
        }
        self.gate.on_frame(&frame);
        let inactivity_ms = if self.last_speech_ms == 0 && self.desktop.is_some() {
            15_000
        } else {
            self.inactivity_ms
        };
        if frame.t_end_ms.saturating_sub(self.last_speech_ms) >= inactivity_ms
            || frame.t_end_ms >= self.max_ms
        {
            return None;
        }
        Some(frame)
    }

    fn on_text(&mut self, _input_ms: i64, committed: &str, tentative: &str) {
        if self.cues_enabled {
            self.gate.on_text(committed);
        }
        if let Some(desktop) = self.desktop {
            desktop.publish(crate::desktop::safe_committed(
                committed,
                self.cues_enabled.then_some(&self.cue_config),
            ));
        }
        let text = format!("{committed}{tentative}");
        if text != self.last_emitted {
            self.last_emitted = text.clone();
            emit(&self.app, SessionEvent::Interim { text });
        }
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

// ---------------------------------------------------------------------------
// VAD-gated mode
// ---------------------------------------------------------------------------

struct SegmentHooks<'a> {
    frames: std::collections::vec_deque::IntoIter<AudioFrame>,
    microphone: &'a mut Microphone,
    app: &'a AppHandle,
    prefix: &'a str,
    stop: &'a Arc<AtomicBool>,
    cancel: &'a Arc<AtomicBool>,
    boundary_ms: u64,
    start_ms: u64,
    end_ms: u64,
    input_ms: u64,
    max_ms: u64,
    ended: bool,
    capture_error: Option<String>,
    desktop: Option<&'a DesktopStream>,
    cue_config: Option<&'a crate::cue::CueConfig>,
}

impl StreamHooks for SegmentHooks<'_> {
    fn next_frame(&mut self) -> Option<AudioFrame> {
        if self.cancel.load(Ordering::Relaxed) {
            return None;
        }
        if let Some(frame) = self.frames.next() {
            return Some(frame);
        }
        if self.ended {
            return None;
        }
        let frame = match self.microphone.read_frame(self.stop) {
            Ok(Some(frame)) => frame,
            Ok(None) => return None,
            Err(message) => {
                self.capture_error = Some(message);
                return None;
            }
        };
        self.input_ms = frame.t_end_ms;
        if frame.speech {
            self.end_ms = frame.t_end_ms;
        }
        // Feed short pauses too: dropping them distorts word timing and cues.
        self.ended = utterance_ended(
            self.start_ms,
            self.end_ms,
            self.input_ms,
            self.boundary_ms,
            self.max_ms,
        );
        Some(frame)
    }

    fn on_text(&mut self, _input_ms: i64, committed: &str, tentative: &str) {
        if let Some(desktop) = self.desktop {
            let safe = crate::desktop::safe_committed(committed, self.cue_config);
            desktop.publish(&join(&[self.prefix, safe]));
        }
        emit(
            self.app,
            SessionEvent::Interim {
                text: join(&[self.prefix, committed, tentative]),
            },
        );
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

fn join(parts: &[&str]) -> String {
    let mut out = String::new();
    for part in parts {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(part);
    }
    out
}

// ---------------------------------------------------------------------------
// Session entry point
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
pub fn run(
    app: AppHandle,
    stop: Arc<AtomicBool>,
    cancel: Arc<AtomicBool>,
    abort: transcribe_cpp::CancelToken,
    running: Arc<AtomicBool>,
    settings: Settings,
    cancel_registered: bool,
    wait_for_focus: bool,
) {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_inner(&app, &stop, &cancel, &abort, &settings, wait_for_focus)
    }));
    let result = result.unwrap_or_else(|_| {
        Err("Dictation worker failed. Restart the app before retrying.".into())
    });
    let state = match result {
        _ if cancel.load(Ordering::Relaxed) => {
            emit(
                &app,
                SessionEvent::Final {
                    text: String::new(),
                    action: Some("Cancel".into()),
                },
            );
            "cancelled"
        }
        Ok(state) => state,
        Err(message) => {
            if let Ok(mut checks) = app.state::<crate::app::AppState>().setup_checks.lock() {
                checks.microphone = None; // recheck after an input/session failure
            }
            emit(&app, SessionEvent::Error { message });
            "error"
        }
    };
    if cancel_registered {
        crate::shortcuts::unregister_cancel(&app, &settings.cancel_key);
    }
    running.store(false, Ordering::Release);
    app.state::<crate::app::AppState>()
        .interactive_wanted
        .store(false, Ordering::Release);
    emit(
        &app,
        SessionEvent::State {
            state: state.into(),
        },
    );
}

/// Eagerly load the model so the app is normally never without one.
/// Idempotent: no-op when a load is in flight or the model is already ready.
pub fn preload(app: &AppHandle) {
    use crate::app::{AppState, ModelStatus};
    let state = app.state::<AppState>();
    if *state.model_status.lock().unwrap_or_else(|p| p.into_inner()) == ModelStatus::Ready {
        return;
    }
    if state
        .model_loading
        .swap(true, std::sync::atomic::Ordering::AcqRel)
    {
        return;
    }
    let handle = app.clone();
    let spawned = std::thread::Builder::new()
        .name("model-preload".into())
        .stack_size(crate::engine::ENGINE_THREAD_STACK_BYTES)
        .spawn(move || {
            let app = handle;
            let state = app.state::<AppState>();
            let settings = state
                .settings
                .lock()
                .map_err(|_| "settings lock poisoned".to_string())
                .map(|s| s.clone());
            let result = (|| -> Result<(), String> {
                let settings = settings?;
                if !Path::new(&settings.model_path).exists() {
                    crate::app::set_model_status(&app, ModelStatus::Missing, None);
                    return Ok(());
                }
                crate::app::set_model_status(&app, ModelStatus::Loading, None);
                let mut cache = state
                    .engine
                    .lock()
                    .map_err(|_| "model lock poisoned; restart the app".to_string())?;
                load_cached(&mut cache, &settings)?;
                crate::app::set_model_status(
                    &app,
                    ModelStatus::Ready,
                    Some(crate::model_store::MODEL_NAME),
                );
                Ok(())
            })();
            if let Err(message) = result {
                crate::app::set_model_status(&app, ModelStatus::Error, None);
                eprintln!("arxell: model preload failed: {message}");
            }
            state
                .model_loading
                .store(false, std::sync::atomic::Ordering::Release);
        });
    if spawned.is_err() {
        state
            .model_loading
            .store(false, std::sync::atomic::Ordering::Release);
    }
}

pub fn load_cached(cache: &mut crate::app::ModelCache, settings: &Settings) -> Result<(), String> {
    if cache.path != settings.model_path || cache.backend != settings.advanced.backend {
        cache.engine = None;
    }
    if cache.engine.is_none() {
        cache.engine = Some(Engine::load_verified(
            Path::new(&settings.model_path),
            DEFAULT_MODEL_SHA256,
            Some(DEFAULT_MODEL_SIZE),
            settings.advanced.backend,
        )?);
        cache.path = settings.model_path.clone();
        cache.backend = settings.advanced.backend;
    }
    cache.last_used = Some(std::time::Instant::now());
    Ok(())
}

fn run_inner(
    app: &AppHandle,
    stop: &Arc<AtomicBool>,
    cancel: &Arc<AtomicBool>,
    abort: &transcribe_cpp::CancelToken,
    settings: &Settings,
    wait_for_focus: bool,
) -> Result<&'static str, String> {
    settings.validate()?;
    if stop.load(Ordering::Relaxed) {
        return Ok("cancelled");
    }
    emit(
        app,
        SessionEvent::State {
            state: "loading".into(),
        },
    );

    // Both focus discovery and capture start before waiting on a preempted API
    // inference. Audio queues remain bounded; focus discovery never gates mic IO.
    let mut desktop = if settings.advanced.desktop_enabled {
        match DesktopStream::start(
            app.clone(),
            cancel.clone(),
            wait_for_focus,
            settings.advanced.terminal_enabled,
        ) {
            Ok(stream) => Some(stream),
            Err(message) => {
                emit(app, SessionEvent::Warning { message });
                None
            }
        }
    } else {
        None
    };
    let microphone = Microphone::open_session(
        Some(&settings.microphone),
        settings.advanced.vad_threshold,
        settings.advanced.speech_hold_ms,
        stop.clone(),
    )?;
    emit(
        app,
        SessionEvent::State {
            state: "listening".into(),
        },
    );

    let state = app.state::<crate::app::AppState>();
    let mut cache = state
        .engine
        .lock()
        .map_err(|_| "model lock poisoned; restart the app")?;
    if cache.engine.is_none()
        || cache.path != settings.model_path
        || cache.backend != settings.advanced.backend
    {
        crate::app::set_model_status(app, crate::app::ModelStatus::Missing, None);
        preload(app);
        return Err("The selected model changed or was unloaded. Wait for Ready, then start a new dictation.".into());
    }
    load_cached(&mut cache, settings)?;
    crate::app::set_model_status(
        app,
        crate::app::ModelStatus::Ready,
        Some(crate::model_store::MODEL_NAME),
    );
    let engine = cache.engine.as_mut().ok_or("model not loaded")?;
    let info = engine.info();
    emit(
        app,
        SessionEvent::Backend {
            backend: info.backend,
            device: info.device,
        },
    );

    if stop.load(Ordering::Relaxed) {
        return Ok("cancelled");
    }

    let started = Instant::now();
    let preset = settings.advanced.stream_preset;
    let result = if settings.advanced.vad_gating {
        run_gated(
            app,
            engine,
            microphone,
            settings,
            preset,
            abort,
            stop,
            cancel,
            desktop.as_ref(),
        )
    } else {
        run_continuous(
            app,
            engine,
            microphone,
            settings,
            preset,
            abort,
            stop,
            cancel,
            desktop.as_ref(),
        )
    }?;
    let wall_ms = started.elapsed().as_millis() as u64;
    let cancelled = cancel.load(Ordering::Relaxed) || result.cue == Some(CueAction::Cancel);
    let action = result.cue.map(|action| format!("{action:?}"));
    emit(
        app,
        SessionEvent::State {
            state: "finalizing".into(),
        },
    );

    if settings.advanced.diagnostics {
        let rtf = if wall_ms == 0 {
            0.0
        } else {
            result.fed_ms as f64 / wall_ms as f64
        };
        eprintln!(
            "arxell session: preset={} gated={} segments={} fed_ms={} wall_ms={} rtf={rtf:.2} max_feed_ms={} stable={}",
            preset.label(),
            settings.advanced.vad_gating,
            result.segments,
            result.fed_ms,
            wall_ms,
            result.max_feed_ms,
            result.stable
        );
        if rtf > 0.9 && !cancelled {
            emit(
                app,
                SessionEvent::Warning {
                    message: format!(
                        "Processing is near real time ({:.0}%). Choose a faster preset in Settings if sessions stall.",
                        rtf * 100.0
                    ),
                },
            );
        }
    }
    if !result.stable && !cancelled {
        emit(app, SessionEvent::Warning { message: "Recognition changed during finalization. Final text was kept; no cue action was applied.".into() });
    }

    let text = if cancelled {
        String::new()
    } else {
        result.text
    };
    if !cancelled
        && result.capture_error.is_none()
        && let Some(stream) = desktop.as_mut()
    {
        stream.finish(&text);
    }
    drop(desktop);
    emit(app, SessionEvent::Final { text, action });
    cache.last_used = Some(std::time::Instant::now());
    drop(cache);
    if let Some(error) = result.capture_error {
        return Err(error);
    }
    Ok(if cancelled { "cancelled" } else { "complete" })
}

// ---------------------------------------------------------------------------
// Mode implementations
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn run_gated(
    app: &AppHandle,
    engine: &mut Engine,
    mut microphone: Microphone,
    settings: &Settings,
    preset: StreamPreset,
    abort: &transcribe_cpp::CancelToken,
    stop: &Arc<AtomicBool>,
    cancel: &Arc<AtomicBool>,
    desktop: Option<&DesktopStream>,
) -> Result<RunResult, String> {
    let cue = &settings.advanced.cue;
    let mut result = RunResult::default();
    // Split at the short pre-cue pause; hold a cue candidate until the longer
    // confirmation pause completes.
    let boundary = cue.pre_gap_ms.clamp(1, cue.confirm_ms.saturating_sub(1));
    let mut preroll = VecDeque::new();
    let mut session_text = String::new();
    let mut pending: Option<(CueAction, String, u64)> = None;
    let mut previous_end_ms: Option<u64> = None;
    let mut last_speech_ms = 0;
    let max_ms = settings.advanced.max_session_seconds * 1_000;

    loop {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let frame = match microphone.read_frame(stop) {
            Ok(Some(frame)) => frame,
            Ok(None) => break,
            Err(message) => {
                result.capture_error = Some(message);
                break;
            }
        };
        let mut input_ms = frame.t_end_ms;
        if frame.speech {
            if let Some((_, text, _)) = pending.take() {
                append(&mut session_text, &text, app);
            }
            let start_ms = frame.t_start_ms;
            let pre_gap_ms = previous_end_ms.map_or(u64::MAX, |end| start_ms.saturating_sub(end));
            last_speech_ms = frame.t_end_ms;
            preroll.push_back(frame);
            let mut hooks = SegmentHooks {
                frames: std::mem::take(&mut preroll).into_iter(),
                microphone: &mut microphone,
                app,
                prefix: &session_text,
                stop,
                cancel,
                boundary_ms: boundary,
                start_ms,
                end_ms: last_speech_ms,
                input_ms,
                max_ms,
                ended: false,
                capture_error: None,
                desktop,
                cue_config: settings.advanced.cues_enabled.then_some(cue),
            };
            // Recognition now consumes live capture, not a completed phrase.
            let decode_preset = if result.segments == 0 {
                preset.interactive_startup()
            } else {
                preset
            };
            let outcome = engine.run_stream_cancellable(&mut hooks, decode_preset, abort)?;
            input_ms = hooks.input_ms;
            last_speech_ms = hooks.end_ms;
            previous_end_ms = Some(hooks.end_ms);
            let duration_ms = hooks.end_ms.saturating_sub(start_ms);
            result.capture_error = hooks.capture_error.take();
            result.segments += 1;
            result.fed_ms += outcome.fed_ms;
            result.max_feed_ms = result.max_feed_ms.max(outcome.max_feed_ms);
            let stable = outcome.committed_mutations == 0 && !outcome.divergent;
            result.stable &= stable;
            let text = outcome.final_text.trim();
            if !text.is_empty() {
                let action = if settings.advanced.cues_enabled && stable {
                    cue.action_for(text)
                        .filter(|_| duration_ms <= cue.max_cue_ms && pre_gap_ms >= cue.pre_gap_ms)
                } else {
                    None
                };
                match action {
                    Some(action) => pending = Some((action, text.to_owned(), last_speech_ms)),
                    None => append(&mut session_text, text, app),
                }
            }
        } else {
            preroll.push_back(frame);
            while preroll
                .front()
                .is_some_and(|first| input_ms.saturating_sub(first.t_start_ms) > 400)
            {
                preroll.pop_front();
            }
        }
        if let Some((action, text, end_ms)) = pending.take() {
            if input_ms.saturating_sub(end_ms) >= cue.confirm_ms {
                result.cue = Some(action);
                break;
            }
            pending = Some((action, text, end_ms));
        }
        if let Some(desktop) = desktop {
            desktop.publish(&session_text);
        }
        let inactivity_ms = if last_speech_ms == 0 && desktop.is_some() {
            15_000
        } else {
            cue.inactivity_ms
        };
        if result.capture_error.is_some()
            || input_ms.saturating_sub(last_speech_ms) >= inactivity_ms
            || input_ms >= max_ms
        {
            break;
        }
    }
    if let Some((_, text, _)) = pending.take() {
        append(&mut session_text, &text, app);
    }
    result.text = session_text;
    Ok(result)
}

fn utterance_ended(
    start_ms: u64,
    end_ms: u64,
    input_ms: u64,
    boundary_ms: u64,
    max_ms: u64,
) -> bool {
    input_ms.saturating_sub(end_ms) >= boundary_ms
        || end_ms.saturating_sub(start_ms) >= 20_000
        || input_ms >= max_ms
}

fn append(session_text: &mut String, text: &str, app: &AppHandle) {
    if text.is_empty() {
        return;
    }
    if !session_text.is_empty() {
        session_text.push(' ');
    }
    session_text.push_str(text);
    emit(
        app,
        SessionEvent::Interim {
            text: session_text.clone(),
        },
    );
}

#[allow(clippy::too_many_arguments)]
fn run_continuous(
    app: &AppHandle,
    engine: &mut Engine,
    microphone: Microphone,
    settings: &Settings,
    preset: StreamPreset,
    abort: &transcribe_cpp::CancelToken,
    stop: &Arc<AtomicBool>,
    cancel: &Arc<AtomicBool>,
    desktop: Option<&DesktopStream>,
) -> Result<RunResult, String> {
    // Continuous cue detection is text-based; wait long enough for the
    // recognizer's lookahead so a cue is not missed.
    let mut cue_config = settings.advanced.cue.clone();
    cue_config.text_grace_ms = cue_config.text_grace_ms.max(preset.lookahead_ms() + 300);
    let mut hooks = ContinuousHooks {
        microphone: Some(microphone),
        gate: CueGate::new(cue_config),
        last_speech_ms: 0,
        inactivity_ms: settings.advanced.cue.inactivity_ms,
        stop: stop.clone(),
        cancel: cancel.clone(),
        app: app.clone(),
        last_emitted: String::new(),
        capture_error: None,
        cues_enabled: settings.advanced.cues_enabled,
        max_ms: settings.advanced.max_session_seconds * 1_000,
        recent_audio: VecDeque::new(),
        recent_ms: settings.advanced.cue.max_cue_ms + settings.advanced.cue.confirm_ms + 1_000,
        desktop,
        cue_config: settings.advanced.cue.clone(),
    };
    let outcome = engine.run_stream_cancellable(&mut hooks, preset, abort)?;
    hooks.microphone.take();
    let stable = outcome.committed_mutations == 0 && !outcome.divergent;
    let filtered = if stable {
        hooks.gate.filtered_final(&outcome.final_text)
    } else {
        None
    };
    let candidate = filtered
        .as_ref()
        .filter(|_| hooks.capture_error.is_none())
        .and_then(|_| hooks.gate.action());
    let cue = if let (Some(action), Some(interval)) = (candidate, hooks.gate.action_interval()) {
        let pcm: Vec<f32> = hooks
            .recent_audio
            .iter()
            .filter(|frame| {
                frame.t_end_ms > interval.start_ms.saturating_sub(64)
                    && frame.t_start_ms < interval.end_ms + 64
            })
            .flat_map(|frame| frame.pcm.iter().copied())
            .collect();
        if !pcm.is_empty()
            && settings
                .advanced
                .cue
                .action_for(&engine.transcribe_cancellable(&pcm, abort)?)
                == Some(action)
        {
            Some(action)
        } else {
            emit(app, SessionEvent::Warning { message: "Spoken control could not be confirmed from its audio. Text was retained and no submission was attempted.".into() });
            None
        }
    } else {
        None
    };
    let text = if cue.is_some() {
        filtered.unwrap_or(outcome.final_text)
    } else {
        outcome.final_text
    };
    Ok(RunResult {
        text,
        cue,
        stable,
        capture_error: hooks.capture_error.take(),
        segments: 1,
        fed_ms: outcome.fed_ms,
        max_feed_ms: outcome.max_feed_ms,
    })
}

pub(crate) fn emit(app: &AppHandle, event: SessionEvent) {
    #[derive(Clone, Serialize)]
    struct Event {
        session_id: u64,
        revision: u64,
        #[serde(flatten)]
        event: SessionEvent,
    }
    let app_state = app.state::<crate::app::AppState>();
    // A delivery event must not overtake a final/state event and make the UI
    // discard it as an old revision. Hold the order lock through emission.
    let _order = app_state
        .session_events
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let session_id = app_state.next_session.load(Ordering::Acquire);
    let mut first_text_ms = None;
    let revision = if let Ok(mut snapshot) = app_state.snapshot.lock() {
        if snapshot.session_id != session_id {
            snapshot.text.clear();
            snapshot.started_at = Some(Instant::now());
            snapshot.first_text_emitted = false;
        }
        snapshot.session_id = session_id;
        snapshot.revision += 1;
        match &event {
            SessionEvent::State { state } => snapshot.state = state.clone(),
            SessionEvent::Interim { text } | SessionEvent::Final { text, .. } => {
                if !text.trim().is_empty() && !snapshot.first_text_emitted {
                    snapshot.first_text_emitted = true;
                    first_text_ms = snapshot.started_at.map(|start| start.elapsed().as_millis());
                }
                snapshot.text = text.clone()
            }
            SessionEvent::Backend { backend, .. } => snapshot.backend = backend.clone(),
            _ => {}
        }
        snapshot.revision
    } else {
        0
    };
    if let Some(ms) = first_text_ms {
        crate::app::trace(format_args!(
            "first_text session={session_id} start_to_text_ms={ms}"
        ));
    }
    match &event {
        SessionEvent::State { state } => {
            crate::app::trace(format_args!("session id={session_id} state={state}"))
        }
        SessionEvent::Delivery { state, .. } => {
            crate::app::trace(format_args!("delivery session={session_id} state={state}"))
        }
        SessionEvent::Final { text, .. } => crate::app::trace(format_args!(
            "session id={session_id} final_characters={}",
            text.chars().count()
        )),
        SessionEvent::Warning { .. } => {
            crate::app::trace(format_args!("session id={session_id} warning=true"))
        }
        SessionEvent::Error { .. } => {
            crate::app::trace(format_args!("session id={session_id} error=true"))
        }
        _ => {}
    }
    if let Some(ms) = first_text_ms
        && app_state
            .settings
            .lock()
            .is_ok_and(|settings| settings.advanced.diagnostics)
    {
        eprintln!("arxell first_text: session={session_id} start_to_text_ms={ms}");
    }
    let _ = app.emit(
        "session-event",
        Event {
            session_id,
            revision,
            event,
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cue::CueConfig;

    fn frame(seq: u64, start: u64, end: u64, speech: bool) -> AudioFrame {
        AudioFrame {
            seq,
            t_start_ms: start,
            t_end_ms: end,
            pcm: vec![0.0; 256],
            speech,
        }
    }

    #[test]
    fn live_utterance_keeps_short_pauses_and_stops_at_boundary_or_limit() {
        // Speech and short pauses must continue feeding the SAME live stream.
        assert!(!utterance_ended(400, 1_000, 1_000, 250, 600_000));
        assert!(!utterance_ended(400, 1_000, 1_240, 250, 600_000));
        assert!(utterance_ended(400, 1_000, 1_256, 250, 600_000));
        assert!(utterance_ended(400, 20_400, 20_400, 250, 600_000));
        assert!(utterance_ended(400, 1_000, 1_000, 250, 1_000));
    }

    #[test]
    fn gated_collector_splits_only_after_confirmation_pause() {
        let mut collector = SegmentCollector::new(800, 400, 20_000);
        // 500 ms speech, 300 ms gap, 500 ms speech: one segment.
        let mut time = 0;
        for _ in 0..31 {
            collector.push(&frame(0, time, time + 16, true));
            time += 16;
        }
        for _ in 0..19 {
            assert!(collector.push(&frame(0, time, time + 16, false)).is_none());
            time += 16;
        }
        for _ in 0..31 {
            assert!(collector.push(&frame(0, time, time + 16, true)).is_none());
            time += 16;
        }
        let mut ended = None;
        for _ in 0..60 {
            if let Some(segment) = collector.push(&frame(0, time, time + 16, false)) {
                ended = Some(segment);
                break;
            }
            time += 16;
        }
        let segment = ended.expect("segment should end after 800 ms silence");
        assert!(segment.duration_ms() > 1_200);
    }

    #[test]
    fn gated_collector_includes_preroll_before_speech() {
        let mut collector = SegmentCollector::new(800, 400, 20_000);
        for i in 0..10 {
            collector.push(&frame(0, i * 16, (i + 1) * 16, false));
        }
        collector.push(&frame(0, 160, 176, true));
        let segment = collector.finish().expect("open segment");
        assert!(segment.frames.len() > 1, "pre-roll should precede speech");
        assert_eq!(segment.start_ms, 160);
    }

    #[test]
    fn gated_cue_requires_boundary_and_short_duration() {
        let config = CueConfig::default();
        let cue = Segment {
            frames: vec![],
            start_ms: 1_000,
            end_ms: 1_400,
            pre_gap_ms: 900,
        };
        assert!(cue.duration_ms() <= config.max_cue_ms);
        assert!(cue.pre_gap_ms >= config.pre_gap_ms);
        let embedded = Segment {
            start_ms: 0,
            end_ms: 3_000,
            pre_gap_ms: 900,
            ..cue
        };
        assert!(embedded.duration_ms() > config.max_cue_ms);
    }
}
