//! Microphone smoke test. This OPENS THE DEFAULT INPUT DEVICE for ~2 seconds.
//!
//!     cargo run --example mic_smoke
//!
//! It records nothing to disk and exits after 125 frames.

use std::error::Error;

use arxell_speak_lib::capture::CaptureConfig;
use arxell_speak_lib::mic::Microphone;

fn main() -> Result<(), Box<dyn Error>> {
    let mut microphone = Microphone::open(CaptureConfig::default())?;
    let mut frames = 0usize;
    let mut speech_frames = 0usize;

    let mut duration_ms = 0;
    while let Some(frame) = microphone.read_frame(&std::sync::atomic::AtomicBool::new(false))? {
        duration_ms = frame.t_end_ms;
        frames += 1;
        if frame.speech {
            speech_frames += 1;
        }
        if frames >= 125 {
            break;
        }
    }

    println!(
        "frames={} duration_ms={} speech_frames={}",
        frames, duration_ms, speech_frames
    );
    Ok(())
}
