//! Continuous resampling and speech detection, outside the audio callback.

use rubato::{
    Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};
use std::collections::VecDeque;

use crate::capture::AudioFrame;

pub const RATE: u32 = 16_000;
pub const FRAME: usize = 256;
const INPUT: usize = 512;

pub struct AudioProcessor {
    resampler: Option<SincFixedIn<f32>>,
    input: Vec<f32>,
    output: VecDeque<f32>,
    detector: Box<earshot::Detector>,
    threshold: f32,
    hold_frames: u64,
    last_voice: Option<u64>,
    seq: u64,
    skip: usize,
    input_rate: u32,
    input_seen: u64,
    output_seen: u64,
    finished: bool,
}

impl AudioProcessor {
    pub fn new(rate: u32, threshold: f32, hold_ms: u64) -> Result<Self, String> {
        if !(8_000..=192_000).contains(&rate)
            || !threshold.is_finite()
            || !(0.05..=0.95).contains(&threshold)
        {
            return Err("unsupported sample rate or speech sensitivity".into());
        }
        let resampler = if rate == RATE {
            None
        } else {
            Some(
                SincFixedIn::new(
                    RATE as f64 / rate as f64,
                    1.0,
                    SincInterpolationParameters {
                        sinc_len: 128,
                        f_cutoff: 0.95,
                        interpolation: SincInterpolationType::Linear,
                        oversampling_factor: 128,
                        window: WindowFunction::BlackmanHarris2,
                    },
                    INPUT,
                    1,
                )
                .map_err(|e| e.to_string())?,
            )
        };
        let skip = resampler.as_ref().map_or(0, |r| r.output_delay());
        Ok(Self {
            resampler,
            input: Vec::with_capacity(INPUT),
            output: VecDeque::new(),
            detector: earshot::Detector::default_boxed(),
            threshold,
            hold_frames: hold_ms.div_ceil(16),
            last_voice: None,
            seq: 0,
            skip,
            input_rate: rate,
            input_seen: 0,
            output_seen: 0,
            finished: false,
        })
    }

    pub fn push(&mut self, samples: &[f32]) -> Result<(), String> {
        if self.finished {
            return Err("audio processor already finalized".into());
        }
        self.input_seen += samples.len() as u64;
        for &sample in samples {
            let sample = if sample.is_finite() {
                sample.clamp(-1.0, 1.0)
            } else {
                0.0
            };
            if let Some(resampler) = self.resampler.as_mut() {
                self.input.push(sample);
                if self.input.len() == INPUT {
                    let output = resampler
                        .process(&[&self.input], None)
                        .map_err(|e| e.to_string())?;
                    self.input.clear();
                    let skip = self.skip.min(output[0].len());
                    self.skip -= skip;
                    self.output.extend(output[0][skip..].iter().copied());
                }
            } else {
                self.output.push_back(sample);
            }
        }
        Ok(())
    }

    pub fn finish(&mut self) -> Result<(), String> {
        if self.finished {
            return Ok(());
        }
        let target = (self.input_seen * RATE as u64).div_ceil(self.input_rate as u64);
        while self.output_seen + (self.output.len() as u64) < target {
            let Some(resampler) = self.resampler.as_mut() else {
                break;
            };
            self.input.resize(INPUT, 0.0);
            let output = resampler
                .process(&[&self.input], None)
                .map_err(|e| e.to_string())?;
            self.input.clear();
            let skip = self.skip.min(output[0].len());
            self.skip -= skip;
            self.output.extend(output[0][skip..].iter().copied());
        }
        self.output
            .truncate(target.saturating_sub(self.output_seen) as usize);
        let padding = (FRAME - self.output.len() % FRAME) % FRAME;
        self.output.extend(std::iter::repeat_n(0.0, padding));
        self.finished = true;
        Ok(())
    }

    pub fn next_frame(&mut self) -> Option<AudioFrame> {
        if self.output.len() < FRAME {
            return None;
        }
        let pcm: Vec<f32> = self.output.drain(..FRAME).collect();
        if self.detector.predict_f32(&pcm) >= self.threshold {
            self.last_voice = Some(self.seq);
        }
        let speech = self
            .last_voice
            .is_some_and(|last| self.seq - last <= self.hold_frames);
        let frame = AudioFrame {
            seq: self.seq,
            t_start_ms: self.seq * 16,
            t_end_ms: (self.seq + 1) * 16,
            pcm,
            speech,
        };
        self.seq += 1;
        self.output_seen += FRAME as u64;
        Some(frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finalization_preserves_partial_frames_and_filter_tail() {
        for rate in [8_000, 16_000, 44_100, 48_000] {
            let count = rate as usize + 137;
            let mut audio = AudioProcessor::new(rate, 0.5, 0).unwrap();
            let mut samples = 0;
            for part in vec![0.1; count].chunks(111) {
                audio.push(part).unwrap();
                while let Some(frame) = audio.next_frame() {
                    samples += frame.pcm.len();
                }
            }
            audio.finish().unwrap();
            while let Some(frame) = audio.next_frame() {
                samples += frame.pcm.len();
            }
            let expected = (count * 16000).div_ceil(rate as usize).div_ceil(FRAME) * FRAME;
            assert_eq!(samples, expected, "sample rate {rate}");
            audio.finish().unwrap();
            assert!(audio.next_frame().is_none());
        }
    }

    #[test]
    fn silence_is_not_speech_and_timestamps_are_exact() {
        let mut audio = AudioProcessor::new(RATE, 0.5, 64).unwrap();
        audio.push(&vec![0.0; FRAME * 100]).unwrap();
        for i in 0..100 {
            let frame = audio.next_frame().unwrap();
            assert!(!frame.speech);
            assert_eq!((frame.t_start_ms, frame.t_end_ms), (i * 16, (i + 1) * 16));
        }
        assert!(audio.next_frame().is_none());
    }

    #[test]
    fn callback_boundaries_do_not_change_resampling() {
        let input: Vec<f32> = (0..48_000).map(|i| (i as f32 * 0.13).sin() * 0.2).collect();
        let collect = |chunk: usize| {
            let mut audio = AudioProcessor::new(48_000, 0.5, 0).unwrap();
            let mut result = Vec::new();
            for part in input.chunks(chunk) {
                audio.push(part).unwrap();
                while let Some(frame) = audio.next_frame() {
                    result.extend(frame.pcm);
                }
            }
            result
        };
        let expected = collect(512);
        assert_eq!(expected, collect(137));
        assert!(expected.len() > 15_000 && expected.len() <= 16_000);
    }

    #[test]
    fn downsampling_suppresses_out_of_band_tones() {
        let energy = |frequency: f32| {
            let mut audio = AudioProcessor::new(48_000, 0.5, 0).unwrap();
            let input: Vec<f32> = (0..48_000)
                .map(|i| (std::f32::consts::TAU * frequency * i as f32 / 48_000.0).sin() * 0.5)
                .collect();
            audio.push(&input).unwrap();
            let mut energy = 0.0;
            while let Some(frame) = audio.next_frame() {
                energy += frame.pcm.iter().map(|x| x * x).sum::<f32>();
            }
            energy
        };
        assert!(energy(12_000.0) < energy(1_000.0) * 0.01);
    }
}
