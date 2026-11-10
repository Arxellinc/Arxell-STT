//! S1 cue isolation: VAD intervals + committed text -> cue resolver.
//!
//!     cargo run --example cue_fixture -- [model.gguf] [cue-fixture.wav] [320|160]
//!
//! Uses a simple energy VAD on the synthetic fixtures. This is a test harness,
//! not the product VAD.

use std::error::Error;
use std::path::Path;

use arxell_speak_lib::capture::{AudioFrame, VadTimeline};
use arxell_speak_lib::cue::{CueConfig, resolve};
use arxell_speak_lib::segmenter::{TextSegmenter, snapshot_at};
use transcribe_cpp::{
    Backend, CommitPolicy, Model, ModelOptions, ParakeetBufferedStreamOptions, RunOptions,
    StreamExtension, StreamOptions,
};

fn load_pcm(path: &Path) -> Result<Vec<f32>, Box<dyn Error>> {
    let mut reader = hound::WavReader::open(path)?;
    let spec = reader.spec();
    if spec.sample_rate != 16_000 || spec.channels != 1 {
        return Err(format!(
            "expected 16 kHz mono, got {} Hz / {} channels",
            spec.sample_rate, spec.channels
        )
        .into());
    }
    let pcm = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<Vec<_>, _>>()?,
        hound::SampleFormat::Int => {
            let scale = (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|sample| sample.map(|value| value as f32 / scale))
                .collect::<Result<Vec<_>, _>>()?
        }
    };
    Ok(pcm)
}

fn rms(frame: &[f32]) -> f32 {
    if frame.is_empty() {
        return 0.0;
    }
    let sum: f32 = frame.iter().map(|sample| sample * sample).sum();
    (sum / frame.len() as f32).sqrt()
}

fn main() -> Result<(), Box<dyn Error>> {
    let model_path = std::env::args().nth(1).unwrap_or_else(|| {
        std::env::var("ARXELL_MODEL").unwrap_or_else(|_| {
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../models/parakeet-unified-en-0.6b-Q4_K_M.gguf"
            )
            .into()
        })
    });
    let audio_path = std::env::args().nth(2).unwrap_or_else(|| {
        concat!(env!("CARGO_MANIFEST_DIR"), "/../tests/fixtures/cue_end.wav").into()
    });
    let preset = std::env::args().nth(3).unwrap_or_else(|| "320".into());
    let (chunk_ms, right_ms) = if preset == "160" {
        (80, 80)
    } else {
        (160, 160)
    };

    let pcm = load_pcm(Path::new(&audio_path))?;
    let audio_end_ms = (pcm.len() as u64) * 1_000 / 16_000;

    let model = Model::load_with(
        Path::new(&model_path),
        &ModelOptions {
            backend: Backend::Auto,
            ..Default::default()
        },
    )?;
    println!(
        "backend: {} | device: {}",
        model.backend(),
        model
            .device()
            .map(|d| d.name)
            .unwrap_or_else(|_| "unknown".into())
    );

    let mut session = model.session()?;
    let options = StreamOptions {
        commit_policy: CommitPolicy::Auto,
        family: Some(StreamExtension::ParakeetBuffered(
            ParakeetBufferedStreamOptions {
                left_ms: Some(5_600),
                chunk_ms: Some(chunk_ms),
                right_ms: Some(right_ms),
            },
        )),
        ..Default::default()
    };
    let mut stream = session.stream(&RunOptions::default(), &options)?;

    let mut timeline = VadTimeline::new();
    let mut snapshots: Vec<(u64, String)> = Vec::new();
    let frame_samples = 320; // 20 ms at 16 kHz
    for (index, frame) in pcm.chunks(frame_samples).enumerate() {
        let start_ms = (index * frame_samples) as u64 * 1_000 / 16_000;
        let end_ms = start_ms + (frame.len() as u64) * 1_000 / 16_000;
        let speech = rms(frame) > 0.003;
        timeline.push(&AudioFrame {
            seq: index as u64,
            t_start_ms: start_ms,
            t_end_ms: end_ms,
            pcm: frame.to_vec(),
            speech,
        });
        let _ = stream.feed(frame)?;
        snapshots.push((end_ms, stream.text().committed));
    }
    timeline.finish();
    let _ = stream.finalize()?;
    let final_text = stream.text().committed;

    let intervals = timeline.intervals().to_vec();
    println!("intervals: {intervals:?}");

    let mut segmenter = TextSegmenter::new();
    for interval in &intervals {
        segmenter.push_interval(*interval, &snapshot_at(&snapshots, interval.start_ms));
    }
    segmenter.finish(&final_text);

    println!("segments:");
    for segment in segmenter.segments() {
        println!(
            "  [{:>5} -> {:>5}] {:?}",
            segment.start_ms, segment.end_ms, segment.text
        );
    }

    let resolution = resolve(segmenter.segments(), &CueConfig::default(), audio_end_ms);
    println!("termination: {:?}", resolution.termination);
    println!("inserted: {:?}", resolution.inserted);
    Ok(())
}
