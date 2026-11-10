//! Pause-delimited utterance grouping over VAD-labelled frames.
//!
//! Segments end after a short silence so a spoken control can be isolated;
//! callers then hold cue candidates until the longer confirmation pause.

use std::collections::VecDeque;

use crate::capture::AudioFrame;

pub struct Segment {
    pub frames: Vec<AudioFrame>,
    pub start_ms: u64,
    pub end_ms: u64,
    pub pre_gap_ms: u64,
}

impl Segment {
    pub fn duration_ms(&self) -> u64 {
        self.end_ms.saturating_sub(self.start_ms)
    }
}

/// Groups speech into pause-delimited segments and keeps a short pre-roll.
pub struct SegmentCollector {
    end_silence_ms: u64,
    preroll_ms: u64,
    max_segment_ms: u64,
    ring: VecDeque<AudioFrame>,
    current: Vec<AudioFrame>,
    in_speech: bool,
    start_ms: u64,
    last_speech_end_ms: u64,
    silence_ms: u64,
    previous_end_ms: Option<u64>,
}

impl SegmentCollector {
    pub fn new(end_silence_ms: u64, preroll_ms: u64, max_segment_ms: u64) -> Self {
        Self {
            end_silence_ms,
            preroll_ms,
            max_segment_ms,
            ring: VecDeque::new(),
            current: Vec::new(),
            in_speech: false,
            start_ms: 0,
            last_speech_end_ms: 0,
            silence_ms: 0,
            previous_end_ms: None,
        }
    }

    pub fn push(&mut self, frame: &AudioFrame) -> Option<Segment> {
        if frame.speech {
            if !self.in_speech {
                self.in_speech = true;
                self.start_ms = frame.t_start_ms;
                self.silence_ms = 0;
                self.current.clear();
                self.current.extend(self.ring.drain(..));
            }
            self.last_speech_end_ms = frame.t_end_ms;
            self.current.push(frame.clone());
            // Bound decode lag and capture backlog for very long monologues.
            if self.last_speech_end_ms.saturating_sub(self.start_ms) >= self.max_segment_ms {
                return self.finish();
            }
            return None;
        }
        self.silence_ms += frame.duration_ms();
        if !self.in_speech {
            self.ring.push_back(frame.clone());
            while self.ring.len() > 1
                && self.ring.front().is_some_and(|first| {
                    frame.t_end_ms.saturating_sub(first.t_start_ms) > self.preroll_ms
                })
            {
                self.ring.pop_front();
            }
            return None;
        }
        if self.silence_ms >= self.end_silence_ms {
            return self.finish();
        }
        None
    }

    pub fn finish(&mut self) -> Option<Segment> {
        if !self.in_speech {
            return None;
        }
        self.in_speech = false;
        self.ring.clear();
        let frames = std::mem::take(&mut self.current);
        let segment = Segment {
            frames,
            start_ms: self.start_ms,
            end_ms: self.last_speech_end_ms,
            pre_gap_ms: self
                .previous_end_ms
                .map_or(u64::MAX, |previous| self.start_ms.saturating_sub(previous)),
        };
        self.previous_end_ms = Some(self.last_speech_end_ms);
        Some(segment)
    }

    pub fn silence_since_speech_ms(&self) -> u64 {
        self.silence_ms
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(start: u64, speech: bool) -> AudioFrame {
        AudioFrame {
            seq: 0,
            t_start_ms: start,
            t_end_ms: start + 16,
            pcm: vec![0.0; 256],
            speech,
        }
    }

    #[test]
    fn long_monologue_splits_at_the_limit() {
        let mut collector = SegmentCollector::new(250, 400, 1_000);
        let mut time = 0;
        let mut ended = None;
        for _ in 0..120 {
            if let Some(segment) = collector.push(&frame(time, true)) {
                ended = Some(segment);
                break;
            }
            time += 16;
        }
        let segment = ended.expect("max segment duration should force a split");
        assert!(segment.duration_ms() >= 1_000 && segment.duration_ms() < 1_100);
    }

    #[test]
    fn short_pause_splits_but_keeps_preroll() {
        let mut collector = SegmentCollector::new(250, 400, 20_000);
        for i in 0..40 {
            collector.push(&frame(i * 16, false));
        }
        for i in 0..20 {
            if let Some(segment) = collector.push(&frame(640 + i * 16, true)) {
                assert_eq!(segment.start_ms, 640);
                return;
            }
        }
        let segment = collector.finish().expect("open segment");
        assert_eq!(segment.start_ms, 640);
        assert!(segment.frames.len() > 20, "preroll should be included");
    }
}
