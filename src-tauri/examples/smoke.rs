//! M1 smoke test: load the selected model and transcribe a 16 kHz mono WAV.
//!
//!     cargo run --example smoke -- [model.gguf] [audio.wav]

use std::error::Error;
use std::path::Path;

use arxell_speak_lib::engine::{
    BackendPreference, DEFAULT_MODEL_SHA256, DEFAULT_MODEL_SIZE, Engine,
};

fn load_pcm(path: &Path) -> Result<Vec<f32>, Box<dyn Error>> {
    let mut reader = hound::WavReader::open(path)?;
    let spec = reader.spec();
    if spec.sample_rate != 16_000 {
        return Err(format!("expected 16 kHz audio, got {} Hz", spec.sample_rate).into());
    }
    if spec.channels != 1 {
        return Err(format!("expected mono audio, got {} channels", spec.channels).into());
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
    let model = std::env::args().nth(1).unwrap_or_else(|| {
        std::env::var("ARXELL_MODEL").unwrap_or_else(|_| {
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../models/parakeet-unified-en-0.6b-Q4_K_M.gguf"
            )
            .into()
        })
    });
    let audio = std::env::args().nth(2).unwrap_or_else(|| {
        concat!(env!("CARGO_MANIFEST_DIR"), "/../tests/fixtures/jfk.wav").into()
    });

    let pcm = load_pcm(Path::new(&audio))?;
    let mut engine = Engine::load_verified(
        Path::new(&model),
        DEFAULT_MODEL_SHA256,
        Some(DEFAULT_MODEL_SIZE),
        BackendPreference::Auto,
    )?;
    let info = engine.info();
    println!("backend: {} | device: {}", info.backend, info.device);
    println!("samples: {}", pcm.len());

    let text = engine.transcribe(&pcm)?;
    println!("text: {}", text.trim());
    Ok(())
}
