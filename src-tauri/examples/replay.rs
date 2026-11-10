//! Recorded-audio test of the production resampler, Earshot and recognizer.

use arxell_speak_lib::{
    audio::AudioProcessor,
    capture::AudioFrame,
    engine::{
        BackendPreference, DEFAULT_MODEL_SHA256, DEFAULT_MODEL_SIZE, Engine, StreamHooks,
        StreamPreset,
    },
};
use std::time::Instant;

struct Replay {
    frames: std::collections::VecDeque<AudioFrame>,
    speech: usize,
    updates: usize,
    started: Instant,
    realtime: bool,
    first_text: Option<(u128, i64)>,
    first_commit: Option<(u128, i64)>,
}
impl StreamHooks for Replay {
    fn next_frame(&mut self) -> Option<AudioFrame> {
        let frame = self.frames.pop_front()?;
        if self.realtime {
            let target = std::time::Duration::from_millis(frame.t_end_ms);
            std::thread::sleep(target.saturating_sub(self.started.elapsed()));
        }
        Some(frame)
    }
    fn on_text(&mut self, ms: i64, committed: &str, tentative: &str) {
        self.updates += 1;
        if self.first_commit.is_none() && !committed.trim().is_empty() {
            let wall_ms = self.started.elapsed().as_millis();
            self.first_commit = Some((wall_ms, ms));
            println!("first_committed: wall_ms={wall_ms} input_ms={ms}");
        }
        if self.first_text.is_none()
            && (!committed.trim().is_empty() || !tentative.trim().is_empty())
        {
            let wall_ms = self.started.elapsed().as_millis();
            self.first_text = Some((wall_ms, ms));
            println!("first_text: wall_ms={wall_ms} input_ms={ms}");
        }
    }
}

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    let path = args
        .get(1)
        .ok_or("usage: replay <mono PCM16 wav> [cpu|gpu] [realtime] [startup]")?;
    let mut wav = hound::WavReader::open(path).map_err(|e| e.to_string())?;
    let spec = wav.spec();
    if spec.channels != 1 || spec.bits_per_sample != 16 {
        return Err("expected mono PCM16 WAV".into());
    }
    let pcm: Result<Vec<f32>, _> = wav
        .samples::<i16>()
        .map(|s| s.map(|s| s as f32 / 32768.0))
        .collect();
    let pcm = pcm.map_err(|e| e.to_string())?;
    let start = Instant::now();
    let mut audio = AudioProcessor::new(spec.sample_rate, 0.5, 0)?;
    let mut replay = Replay {
        frames: Default::default(),
        speech: 0,
        updates: 0,
        started: Instant::now(),
        realtime: args.iter().any(|arg| arg == "realtime"),
        first_text: None,
        first_commit: None,
    };
    for samples in pcm.chunks(512) {
        audio.push(samples)?;
        while let Some(frame) = audio.next_frame() {
            replay.speech += usize::from(frame.speech);
            replay.frames.push_back(frame);
        }
    }
    audio.finish()?;
    while let Some(frame) = audio.next_frame() {
        replay.speech += usize::from(frame.speech);
        replay.frames.push_back(frame);
    }
    let duration_ms = replay.frames.back().map_or(0, |f| f.t_end_ms);
    let first_speech_ms = replay
        .frames
        .iter()
        .find(|f| f.speech)
        .map_or(0, |f| f.t_start_ms);
    println!("first_speech_ms={first_speech_ms}");
    println!(
        "audio: frames={} speech_frames={} duration_ms={duration_ms} preprocessing_ms={}",
        replay.frames.len(),
        replay.speech,
        start.elapsed().as_millis()
    );
    let model = std::env::var("ARXELL_MODEL").map_err(|_| "set ARXELL_MODEL")?;
    let backend = if args.get(2).is_some_and(|s| s == "gpu") {
        BackendPreference::GpuOnly
    } else {
        BackendPreference::CpuOnly
    };
    let start = Instant::now();
    let mut engine = Engine::load_verified(
        std::path::Path::new(&model),
        DEFAULT_MODEL_SHA256,
        Some(DEFAULT_MODEL_SIZE),
        backend,
    )?;
    println!(
        "model: backend={} load_ms={}",
        engine.info().backend,
        start.elapsed().as_millis()
    );
    let start = Instant::now();
    replay.started = start;
    let preset = if args.iter().any(|arg| arg == "startup") {
        StreamPreset::Efficient.interactive_startup()
    } else {
        StreamPreset::Efficient
    };
    let result = engine.run_stream(&mut replay, preset)?;
    println!(
        "stream: updates={} run_ms={} mutations={} divergent={}",
        replay.updates,
        start.elapsed().as_millis(),
        result.committed_mutations,
        result.divergent
    );
    println!("text: {}", result.final_text);
    if replay.realtime
        && replay
            .first_text
            .is_none_or(|(_, ms)| ms as u64 >= duration_ms)
    {
        return Err("no interim text was produced before the audio ended".into());
    }
    if result.committed_mutations != 0 || result.divergent {
        return Err("unstable committed transcript".into());
    }
    Ok(())
}
