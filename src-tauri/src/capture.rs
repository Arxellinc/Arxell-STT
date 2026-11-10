//! Microphone capture, device lifecycle and 16 kHz mono frames.
//!
//! The real device path is platform-specific and not opened in tests. The
//! `CaptureSource` trait lets the session be exercised with a deterministic
//! fake source.

use std::collections::VecDeque;

#[derive(Debug, Clone, Copy)]
pub struct CaptureConfig {
    pub sample_rate_hz: u32,
    pub channels: u16,
    pub frame_ms: u32,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            sample_rate_hz: 16_000,
            channels: 1,
            frame_ms: 16,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AudioFrame {
    pub seq: u64,
    pub t_start_ms: u64,
    pub t_end_ms: u64,
    pub pcm: Vec<f32>,
    pub speech: bool,
}

impl AudioFrame {
    pub fn duration_ms(&self) -> u64 {
        self.t_end_ms.saturating_sub(self.t_start_ms)
    }
}

pub trait CaptureSource {
    fn next_frame(&mut self) -> Option<AudioFrame>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpeechInterval {
    pub start_ms: u64,
    pub end_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VadEvent {
    SpeechStart { start_ms: u64 },
    SpeechEnd(SpeechInterval),
}

/// Pure VAD timeline over labelled frames. The real detector fills `speech`;
/// tests and replay use the same structure.
#[derive(Debug, Default, Clone)]
pub struct VadTimeline {
    in_speech: bool,
    speech_start_ms: u64,
    last_speech_end_ms: u64,
    intervals: Vec<SpeechInterval>,
}

impl VadTimeline {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, frame: &AudioFrame) {
        let _ = self.push_event(frame);
    }

    /// Push a frame and report a speech-interval boundary when one occurs.
    pub fn push_event(&mut self, frame: &AudioFrame) -> Option<VadEvent> {
        if frame.speech {
            if !self.in_speech {
                self.in_speech = true;
                self.speech_start_ms = frame.t_start_ms;
                self.last_speech_end_ms = frame.t_end_ms;
                return Some(VadEvent::SpeechStart {
                    start_ms: frame.t_start_ms,
                });
            }
            self.last_speech_end_ms = frame.t_end_ms;
            None
        } else if self.in_speech {
            self.in_speech = false;
            let interval = SpeechInterval {
                start_ms: self.speech_start_ms,
                end_ms: frame.t_start_ms,
            };
            self.intervals.push(interval);
            Some(VadEvent::SpeechEnd(interval))
        } else {
            None
        }
    }

    pub fn finish(&mut self) {
        if self.in_speech {
            self.in_speech = false;
            self.intervals.push(SpeechInterval {
                start_ms: self.speech_start_ms,
                end_ms: self.last_speech_end_ms,
            });
        }
    }

    pub fn intervals(&self) -> &[SpeechInterval] {
        &self.intervals
    }

    pub fn last_speech_end_ms(&self) -> u64 {
        self.last_speech_end_ms
    }

    pub fn trailing_silence_ms(&self, now_ms: u64) -> u64 {
        if self.in_speech {
            0
        } else {
            now_ms.saturating_sub(self.last_speech_end_ms)
        }
    }
}

#[derive(Debug, Default)]
pub struct FakeCapture {
    frames: VecDeque<AudioFrame>,
}

impl FakeCapture {
    pub fn new(frames: impl IntoIterator<Item = AudioFrame>) -> Self {
        Self {
            frames: frames.into_iter().collect(),
        }
    }
}

impl CaptureSource for FakeCapture {
    fn next_frame(&mut self) -> Option<AudioFrame> {
        self.frames.pop_front()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_capture_preserves_order() {
        let frames = vec![
            AudioFrame {
                seq: 1,
                t_start_ms: 0,
                t_end_ms: 20,
                pcm: vec![0.0; 320],
                speech: false,
            },
            AudioFrame {
                seq: 2,
                t_start_ms: 20,
                t_end_ms: 40,
                pcm: vec![0.0; 320],
                speech: true,
            },
        ];
        let mut source = FakeCapture::new(frames);
        assert_eq!(source.next_frame().unwrap().seq, 1);
        assert_eq!(source.next_frame().unwrap().seq, 2);
        assert!(source.next_frame().is_none());
    }

    #[test]
    fn frame_duration() {
        let frame = AudioFrame {
            seq: 1,
            t_start_ms: 100,
            t_end_ms: 140,
            pcm: Vec::new(),
            speech: true,
        };
        assert_eq!(frame.duration_ms(), 40);
    }

    fn frame(seq: u64, start: u64, end: u64, speech: bool) -> AudioFrame {
        AudioFrame {
            seq,
            t_start_ms: start,
            t_end_ms: end,
            pcm: Vec::new(),
            speech,
        }
    }

    #[test]
    fn vad_timeline_splits_on_silence() {
        let mut timeline = VadTimeline::new();
        timeline.push(&frame(1, 0, 100, true));
        timeline.push(&frame(2, 100, 200, false));
        timeline.push(&frame(3, 200, 300, true));
        timeline.finish();
        assert_eq!(
            timeline.intervals(),
            &[
                SpeechInterval {
                    start_ms: 0,
                    end_ms: 100
                },
                SpeechInterval {
                    start_ms: 200,
                    end_ms: 300
                }
            ]
        );
    }

    #[test]
    fn vad_timeline_tracks_trailing_silence() {
        let mut timeline = VadTimeline::new();
        timeline.push(&frame(1, 0, 200, true));
        timeline.push(&frame(2, 200, 500, false));
        assert_eq!(timeline.trailing_silence_ms(900), 700);
        assert_eq!(timeline.last_speech_end_ms(), 200);
    }

    #[test]
    fn vad_events_mark_boundaries() {
        let mut timeline = VadTimeline::new();
        assert_eq!(
            timeline.push_event(&frame(1, 0, 100, true)),
            Some(VadEvent::SpeechStart { start_ms: 0 })
        );
        assert_eq!(
            timeline.push_event(&frame(2, 100, 200, false)),
            Some(VadEvent::SpeechEnd(SpeechInterval {
                start_ms: 0,
                end_ms: 100
            }))
        );
    }
}
