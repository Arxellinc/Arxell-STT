//! Align committed engine text to VAD speech intervals.
//!
//! The engine returns a growing committed prefix, not cue-sized segments. This
//! segmenter assigns the text delta up to the next speech interval to the
//! previous interval. It is deliberately pure; callers supply the committed
//! snapshot at each interval boundary and at the end of the stream.

use crate::capture::SpeechInterval;
use crate::cue::Segment;

/// Latest committed text known at `time_ms`; never borrow future recognition.
pub fn snapshot_at(snapshots: &[(u64, String)], time_ms: u64) -> String {
    snapshots
        .iter()
        .rev()
        .find(|(end_ms, _)| *end_ms <= time_ms)
        .map(|(_, text)| text.clone())
        .unwrap_or_default()
}

#[derive(Debug, Default, Clone)]
pub struct TextSegmenter {
    intervals: Vec<SpeechInterval>,
    committed: String,
    segments: Vec<Segment>,
}

impl TextSegmenter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a new speech interval. `committed` is the engine's committed
    /// prefix at the moment this interval starts. The previous interval is
    /// finalized with the text committed by that point.
    pub fn push_interval(&mut self, interval: SpeechInterval, committed: &str) {
        if let Some(previous) = self.intervals.last().copied() {
            self.finalize(previous, committed);
        }
        self.intervals.push(interval);
    }

    /// Finish the last interval with the final committed text.
    pub fn finish(&mut self, committed: &str) {
        if let Some(last) = self.intervals.last().copied() {
            self.finalize(last, committed);
        }
        self.intervals.clear();
    }

    pub fn segments(&self) -> &[Segment] {
        &self.segments
    }

    fn finalize(&mut self, interval: SpeechInterval, committed: &str) {
        if !committed.starts_with(&self.committed) {
            // Committed text is append-only; recover by treating the suffix as
            // empty rather than slicing a moved boundary.
            self.committed = committed.to_string();
            return;
        }

        let delta = committed[self.committed.len()..].trim();
        if !delta.is_empty() {
            self.segments.push(Segment {
                text: delta.to_string(),
                start_ms: interval.start_ms,
                end_ms: interval.end_ms,
            });
        }
        self.committed = committed.to_string();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cue::{CueConfig, Termination, resolve};

    #[test]
    fn does_not_assign_future_text_to_an_earlier_boundary() {
        let snapshots = vec![(100, "hello".into()), (900, "hello out".into())];
        assert_eq!(snapshot_at(&snapshots, 50), "");
        assert_eq!(snapshot_at(&snapshots, 500), "hello");
    }

    #[test]
    fn handles_a_rewritten_unicode_prefix() {
        let mut segmenter = TextSegmenter::new();
        let interval = SpeechInterval {
            start_ms: 0,
            end_ms: 100,
        };
        segmenter.push_interval(interval, "");
        segmenter.push_interval(interval, "a");
        segmenter.finish("é out");
        assert_eq!(segmenter.segments().len(), 1);
    }

    #[test]
    fn assigns_text_to_the_previous_interval() {
        let mut segmenter = TextSegmenter::new();
        segmenter.push_interval(
            SpeechInterval {
                start_ms: 0,
                end_ms: 100,
            },
            "",
        );
        segmenter.push_interval(
            SpeechInterval {
                start_ms: 200,
                end_ms: 300,
            },
            "hello",
        );
        segmenter.finish("hello over");

        assert_eq!(
            segmenter.segments(),
            &[
                Segment {
                    text: "hello".into(),
                    start_ms: 0,
                    end_ms: 100,
                },
                Segment {
                    text: "over".into(),
                    start_ms: 200,
                    end_ms: 300,
                }
            ]
        );
    }

    #[test]
    fn continuous_speech_stays_one_segment() {
        let mut segmenter = TextSegmenter::new();
        segmenter.push_interval(
            SpeechInterval {
                start_ms: 0,
                end_ms: 300,
            },
            "",
        );
        segmenter.finish("look over this code");
        assert_eq!(segmenter.segments().len(), 1);
        assert_eq!(segmenter.segments()[0].text, "look over this code");
    }

    #[test]
    fn empty_delta_is_skipped() {
        let mut segmenter = TextSegmenter::new();
        segmenter.push_interval(
            SpeechInterval {
                start_ms: 0,
                end_ms: 100,
            },
            "",
        );
        segmenter.finish("   ");
        assert!(segmenter.segments().is_empty());
    }

    #[test]
    fn isolates_a_cue_between_pauses() {
        let mut segmenter = TextSegmenter::new();
        segmenter.push_interval(
            SpeechInterval {
                start_ms: 0,
                end_ms: 1_000,
            },
            "",
        );
        segmenter.push_interval(
            SpeechInterval {
                start_ms: 1_300,
                end_ms: 1_700,
            },
            "hello",
        );
        segmenter.push_interval(
            SpeechInterval {
                start_ms: 2_700,
                end_ms: 4_000,
            },
            "hello over",
        );
        segmenter.finish("hello over and run");

        assert_eq!(
            segmenter
                .segments()
                .iter()
                .map(|segment| segment.text.as_str())
                .collect::<Vec<_>>(),
            vec!["hello", "over", "and run"]
        );

        let result = resolve(segmenter.segments(), &CueConfig::default(), 4_800);
        assert_eq!(
            result.termination,
            Termination::Cue(crate::cue::CueAction::End)
        );
        // The session ends at the cue; post-cue speech is not part of it.
        assert_eq!(result.inserted, vec!["hello"]);
    }

    #[test]
    fn no_confirmation_pause_keeps_the_cue_as_text() {
        let mut segmenter = TextSegmenter::new();
        segmenter.push_interval(
            SpeechInterval {
                start_ms: 0,
                end_ms: 4_000,
            },
            "",
        );
        segmenter.finish("hello out and run");

        let result = resolve(segmenter.segments(), &CueConfig::default(), 4_800);
        assert_eq!(result.termination, Termination::None);
        assert_eq!(result.inserted, vec!["hello out and run"]);
    }
}
