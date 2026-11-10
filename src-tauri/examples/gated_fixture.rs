//! Replays recorded fixtures through the production VAD-gated pipeline.
//!
//!   ./dev-build.sh example gated_fixture -- <cue-fixture.wav> [gpu|cpu]
//!
//! Uses the real Earshot frames, SegmentCollector, stream preset and cue
//! envelope. Prints each segment and the resulting action so cue behavior can
//! be checked without a microphone.

use arxell_speak_lib::audio::AudioProcessor;
use arxell_speak_lib::capture::AudioFrame;
use arxell_speak_lib::cue::{CueAction, CueConfig, Termination};
use arxell_speak_lib::engine::{
    DEFAULT_MODEL_SHA256, DEFAULT_MODEL_SIZE, Engine, StreamHooks, StreamPreset,
};
use arxell_speak_lib::segments::{Segment, SegmentCollector};

struct Replay {
    frames: std::vec::IntoIter<AudioFrame>,
}

impl StreamHooks for Replay {
    fn next_frame(&mut self) -> Option<AudioFrame> {
        self.frames.next()
    }
    fn on_text(&mut self, _ms: i64, _committed: &str, _tentative: &str) {}
}

fn decode(engine: &mut Engine, segment: &Segment) -> Result<(String, bool), String> {
    let mut replay = Replay {
        frames: segment.frames.clone().into_iter(),
    };
    let outcome = engine.run_stream(&mut replay, StreamPreset::Efficient)?;
    let stable = outcome.committed_mutations == 0 && !outcome.divergent;
    Ok((outcome.final_text.trim().to_string(), stable))
}

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).ok_or("usage: gated_fixture <wav> [gpu|cpu]")?;
    let backend = if args.get(2).is_some_and(|s| s == "cpu") {
        arxell_speak_lib::engine::BackendPreference::CpuOnly
    } else {
        arxell_speak_lib::engine::BackendPreference::Auto
    };
    let mut wav = hound::WavReader::open(path).map_err(|e| e.to_string())?;
    if wav.spec().channels != 1 || wav.spec().bits_per_sample != 16 {
        return Err("expected mono PCM16 WAV".into());
    }
    let pcm: Vec<f32> = wav
        .samples::<i16>()
        .map(|s| s.map(|v| v as f32 / 32768.0))
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;

    let mut audio = AudioProcessor::new(wav.spec().sample_rate, 0.5, 0)?;
    let mut frames = Vec::new();
    for chunk in pcm.chunks(512) {
        audio.push(chunk)?;
        while let Some(frame) = audio.next_frame() {
            frames.push(frame);
        }
    }
    audio.finish()?;
    while let Some(frame) = audio.next_frame() {
        frames.push(frame);
    }

    let model = std::env::var("ARXELL_MODEL").map_err(|_| "set ARXELL_MODEL")?;
    let mut engine = Engine::load_verified(
        std::path::Path::new(&model),
        DEFAULT_MODEL_SHA256,
        Some(DEFAULT_MODEL_SIZE),
        backend,
    )?;
    println!("backend={} frames={}", engine.info().backend, frames.len());

    let cue = CueConfig::default();
    let boundary = cue.pre_gap_ms.max(1);
    let mut collector = SegmentCollector::new(boundary, 400, 20_000);
    let mut held: Option<(CueAction, String, u64)> = None;
    let mut text = String::new();
    let mut action: Option<CueAction> = None;
    let mut index = 0;

    for frame in &frames {
        if let Some(segment) = collector.push(frame) {
            let decoded = decode(&mut engine, &segment)?.0;
            index += 1;
            println!(
                "  segment {index}: {}..{}ms pre_gap={}ms text={decoded:?}",
                segment.start_ms,
                segment.end_ms,
                if segment.pre_gap_ms == u64::MAX {
                    "start".into()
                } else {
                    segment.pre_gap_ms.to_string()
                }
            );
            if decoded.is_empty() {
                continue;
            }
            let candidate = cue.action_for(&decoded).filter(|_| {
                segment.duration_ms() <= cue.max_cue_ms && segment.pre_gap_ms >= cue.pre_gap_ms
            });
            match candidate {
                Some(cue_action) => held = Some((cue_action, decoded, segment.end_ms)),
                None => {
                    if !text.is_empty() {
                        text.push(' ');
                    }
                    text.push_str(&decoded);
                }
            }
        }
        if let Some((cue_action, decoded, end_ms)) = held.take() {
            if frame.speech {
                if !text.is_empty() {
                    text.push(' ');
                }
                text.push_str(&decoded);
            } else if frame.t_end_ms.saturating_sub(end_ms) >= cue.confirm_ms {
                action = Some(cue_action);
                break;
            } else {
                held = Some((cue_action, decoded, end_ms));
            }
        }
    }
    if let Some((_, decoded, _)) = held.take() {
        if !text.is_empty() {
            text.push(' ');
        }
        text.push_str(&decoded);
    }
    if action.is_none()
        && let Some(segment) = collector.finish()
    {
        let decoded = decode(&mut engine, &segment)?.0;
        index += 1;
        println!(
            "  segment {index}: {}..{}ms tail text={decoded:?}",
            segment.start_ms, segment.end_ms
        );
        if !decoded.is_empty() {
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(&decoded);
        }
    }

    let termination = action.map_or(Termination::None, Termination::Cue);
    println!("termination={termination:?} text={text:?}");
    Ok(())
}
