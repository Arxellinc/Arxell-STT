//! S1 partial: streaming stability on a known WAV.
//!
//!     cargo run --example stream_smoke -- [model.gguf] [audio.wav] [320|160]
//!
//! Checks that committed text only grows and that the final hypothesis still
//! starts with the committed prefix. It does not test cue isolation; that needs
//! cue-phrase fixtures.

use std::error::Error;
use std::path::Path;

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
        concat!(env!("CARGO_MANIFEST_DIR"), "/../tests/fixtures/jfk.wav").into()
    });
    let preset = std::env::args().nth(3).unwrap_or_else(|| "320".into());
    let (chunk_ms, right_ms) = if preset == "160" {
        (80, 80)
    } else {
        (160, 160)
    };

    let pcm = load_pcm(Path::new(&audio_path))?;
    let model = Model::load_with(
        Path::new(&model_path),
        &ModelOptions {
            backend: Backend::Auto,
            ..Default::default()
        },
    )?;
    let caps = model.capabilities()?;
    println!(
        "backend: {} | device: {} | streaming: {}",
        model.backend(),
        model
            .device()
            .map(|d| d.name)
            .unwrap_or_else(|_| "unknown".into()),
        caps.supports_streaming
    );
    if !caps.supports_streaming {
        return Err("model does not support streaming".into());
    }

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

    let mut updates = 0usize;
    let mut committed_mutations = 0usize;
    let mut committed = String::new();
    for frame in pcm.chunks(1_600) {
        let update = stream.feed(frame)?;
        if update.committed_changed || update.tentative_changed {
            updates += 1;
        }
        let text = stream.text();
        if !text.committed.starts_with(&committed) {
            committed_mutations += 1;
        }
        committed = text.committed;
    }

    let update = stream.finalize()?;
    let text = stream.text();
    let divergent = !text.full.starts_with(&text.committed);
    println!(
        "preset={} updates={} committed_mutations={} divergent={} final={}",
        preset,
        updates,
        committed_mutations,
        divergent,
        text.full.trim()
    );
    if committed_mutations > 0 || divergent {
        return Err("streaming stability check failed".into());
    }
    if !update.is_final {
        return Err("finalize did not report final".into());
    }
    Ok(())
}
