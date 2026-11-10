//! Live cue gate.
//!
//! Pause-based cue detection for the fallback transcript. Recognition has no
//! word timestamps, so alignment is heuristic and must not authorize desktop
//! submission. Only an unchanged, exact trailing cue may be removed at finish.

use crate::capture::{AudioFrame, SpeechInterval, VadEvent, VadTimeline};
use crate::cue::{CueAction, CueConfig, normalize};

struct Pending {
    interval: SpeechInterval,
    pre_gap_ms: u64,
}

pub struct CueGate {
    config: CueConfig,
    timeline: VadTimeline,
    buffer: String,
    committed: String,
    cue_prefix: Option<String>,
    invalidated: bool,
    pending: Option<Pending>,
    current_pre_gap_ms: u64,
    current_start_ms: u64,
    last_interval_end_ms: Option<u64>,
    action: Option<CueAction>,
    action_interval: Option<SpeechInterval>,
}

impl CueGate {
    pub fn new(config: CueConfig) -> Self {
        Self {
            config,
            timeline: VadTimeline::new(),
            buffer: String::new(),
            committed: String::new(),
            cue_prefix: None,
            invalidated: false,
            pending: None,
            current_pre_gap_ms: u64::MAX,
            current_start_ms: 0,
            last_interval_end_ms: None,
            action: None,
            action_interval: None,
        }
    }

    pub fn on_frame(&mut self, frame: &AudioFrame) {
        if self.action.is_some() {
            return;
        }

        if let Some(event) = self.timeline.push_event(frame) {
            match event {
                VadEvent::SpeechStart { start_ms } => {
                    if let Some(pending) = self.pending.take() {
                        self.last_interval_end_ms = Some(pending.interval.end_ms);
                        if start_ms.saturating_sub(pending.interval.end_ms) < self.config.pre_gap_ms
                        {
                            return;
                        }
                        self.flush();
                    }
                    self.current_pre_gap_ms = match self.last_interval_end_ms {
                        Some(end) => start_ms.saturating_sub(end),
                        None => u64::MAX,
                    };
                    self.current_start_ms = start_ms;
                }
                VadEvent::SpeechEnd(mut interval) => {
                    interval.start_ms = self.current_start_ms;
                    self.pending = Some(Pending {
                        interval,
                        pre_gap_ms: self.current_pre_gap_ms,
                    });
                }
            }
        }

        self.tick(frame.t_end_ms);
    }

    pub fn on_text(&mut self, committed: &str) {
        if !committed.starts_with(&self.committed) {
            self.invalidated = true;
            self.buffer.clear();
            self.action = None;
            self.cue_prefix = None;
            self.committed = committed.into();
            return;
        }
        self.buffer.push_str(&committed[self.committed.len()..]);
        self.committed = committed.into();
    }

    pub fn action(&self) -> Option<CueAction> {
        self.action
    }

    pub fn action_interval(&self) -> Option<SpeechInterval> {
        self.action_interval
    }

    /// Never let a second, heuristic segmentation pass remove arbitrary words.
    /// Strip only the exact trailing cue that caused this gate to stop.
    pub fn filtered_final(&self, final_text: &str) -> Option<String> {
        if self.invalidated || final_text.trim() != self.committed.trim() {
            return None;
        }
        self.cue_prefix.as_ref().map(|s| s.trim().to_string())
    }

    fn tick(&mut self, now_ms: u64) {
        if self.action.is_some() || self.invalidated {
            return;
        }
        let Some(pending) = self.pending.as_ref() else {
            return;
        };
        let elapsed = now_ms.saturating_sub(pending.interval.end_ms);
        if elapsed < self.config.confirm_ms {
            return;
        }

        let interval = pending.interval;
        let pre_gap_ms = pending.pre_gap_ms;
        let duration_ms = interval.end_ms.saturating_sub(interval.start_ms);
        let text = normalize(&self.buffer);

        if !text.is_empty() {
            if let Some(action) = self.config.action_for(&text)
                && duration_ms <= self.config.max_cue_ms
                && pre_gap_ms >= self.config.pre_gap_ms
            {
                self.action = Some(action);
                self.action_interval = Some(interval);
                self.cue_prefix = self
                    .committed
                    .strip_suffix(&self.buffer)
                    .map(str::to_string);
                self.buffer.clear();
                self.pending = None;
                self.last_interval_end_ms = Some(interval.end_ms);
                return;
            }
            self.flush();
            self.pending = None;
            self.last_interval_end_ms = Some(interval.end_ms);
        } else if elapsed >= self.config.confirm_ms + self.config.text_grace_ms {
            self.pending = None;
            self.last_interval_end_ms = Some(interval.end_ms);
        }
    }

    fn flush(&mut self) {
        self.buffer.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_pre_gap_is_not_a_cue() {
        let mut gate = CueGate::new(CueConfig::default());
        gate.on_frame(&frame(0, 0, 1000, true));
        gate.on_text("hello");
        gate.on_frame(&frame(1, 1000, 1100, false));
        gate.on_frame(&frame(2, 1100, 1300, true));
        gate.on_text("hello out");
        gate.on_frame(&frame(3, 1300, 2300, false));
        assert_eq!(gate.action(), None);
    }

    #[test]
    fn unicode_mutation_disables_actions_without_panicking() {
        let mut gate = CueGate::new(CueConfig::default());
        gate.on_text("a");
        gate.on_text("é out");
        gate.on_frame(&frame(0, 0, 100, true));
        gate.on_frame(&frame(1, 100, 1000, false));
        assert_eq!(gate.action(), None);
        assert!(gate.filtered_final("é out").is_none());
    }

    #[test]
    fn filters_only_exact_final_match() {
        let mut gate = CueGate::new(CueConfig::default());
        gate.on_frame(&frame(0, 0, 1000, true));
        gate.on_text("hello");
        gate.on_frame(&frame(1, 1000, 1300, false));
        gate.on_frame(&frame(2, 1300, 1700, true));
        gate.on_text("hello over");
        gate.on_frame(&frame(3, 1700, 2600, false));
        assert_eq!(gate.filtered_final("hello over"), Some("hello".into()));
        assert_eq!(gate.filtered_final("hello over the bridge"), None);
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
    fn detects_end_cue_after_confirmation() {
        let mut gate = CueGate::new(CueConfig::default());
        gate.on_frame(&frame(0, 0, 1_000, true));
        gate.on_text("hello");
        gate.on_frame(&frame(1, 1_000, 1_300, false));
        gate.on_frame(&frame(2, 1_300, 1_700, true));
        gate.on_text("hello over");
        gate.on_frame(&frame(3, 1_700, 2_600, false));
        assert_eq!(gate.action(), Some(CueAction::End));
    }

    #[test]
    fn tiny_speech_gaps_do_not_isolate_an_embedded_cue() {
        let mut gate = CueGate::new(CueConfig::default());
        gate.on_frame(&frame(0, 0, 1000, true));
        gate.on_text("look");
        gate.on_frame(&frame(1, 1000, 1032, false));
        gate.on_frame(&frame(2, 1032, 1200, true));
        gate.on_text("look over");
        gate.on_frame(&frame(3, 1200, 1250, false));
        gate.on_frame(&frame(4, 1250, 1500, true));
        gate.on_text("look over this code");
        gate.on_frame(&frame(5, 1500, 3000, false));
        assert_eq!(gate.action(), None);
    }

    #[test]
    fn no_pre_gap_is_not_a_cue() {
        let mut gate = CueGate::new(CueConfig::default());
        gate.on_frame(&frame(0, 0, 1_500, true));
        gate.on_text("look over this code");
        gate.on_frame(&frame(1, 1_500, 2_500, false));
        assert_eq!(gate.action(), None);
    }

    #[test]
    fn resumed_speech_cancels_pending_cue() {
        let mut gate = CueGate::new(CueConfig::default());
        gate.on_frame(&frame(0, 0, 1_000, true));
        gate.on_text("hello out");
        gate.on_frame(&frame(1, 1_000, 1_200, false));
        gate.on_frame(&frame(2, 1_200, 3_000, true));
        gate.on_text("hello out and run");
        assert_eq!(gate.action(), None);
    }

    #[test]
    fn detects_cancel_cue() {
        let mut gate = CueGate::new(CueConfig::default());
        gate.on_frame(&frame(0, 0, 1_000, true));
        gate.on_text("hello");
        gate.on_frame(&frame(1, 1_000, 1_300, false));
        gate.on_frame(&frame(2, 1_300, 1_700, true));
        gate.on_text("hello cancel");
        gate.on_frame(&frame(3, 1_700, 2_600, false));
        assert_eq!(gate.action(), Some(CueAction::Cancel));
    }

    #[test]
    fn detects_configured_multi_word_submit() {
        let config = CueConfig {
            submit_phrase: "over and out".into(),
            ..Default::default()
        };
        let mut gate = CueGate::new(config);
        gate.on_frame(&frame(0, 0, 1_000, true));
        gate.on_text("hello");
        gate.on_frame(&frame(1, 1_000, 1_300, false));
        gate.on_frame(&frame(2, 1_300, 2_500, true));
        gate.on_text("hello over and out");
        gate.on_frame(&frame(3, 2_500, 3_400, false));
        assert_eq!(gate.action(), Some(CueAction::EndSubmit));
    }
}
