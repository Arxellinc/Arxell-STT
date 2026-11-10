//! Pause-delimited cue resolver.
//!
//! A cue is its own segment: short pause, configured phrase, longer
//! confirmation pause. Matching is exact on the whole normalized segment.
//! The resolver is pure so the deterministic tests can exercise it without a
//! microphone or engine.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CueAction {
    End,
    EndSubmit,
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub text: String,
    pub start_ms: u64,
    pub end_ms: u64,
}

impl Segment {
    pub fn new(text: impl Into<String>, start_ms: u64, end_ms: u64) -> Self {
        Self {
            text: text.into(),
            start_ms,
            end_ms,
        }
    }

    fn duration_ms(&self) -> u64 {
        self.end_ms.saturating_sub(self.start_ms)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CueEvent {
    Insert(String),
    Pending(CueAction),
    Reject(String),
    Action(CueAction),
    Timeout,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Termination {
    Cue(CueAction),
    Timeout,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    pub termination: Termination,
    pub inserted: Vec<String>,
    pub events: Vec<CueEvent>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct CueConfig {
    pub pre_gap_ms: u64,
    pub confirm_ms: u64,
    pub inactivity_ms: u64,
    pub max_cue_ms: u64,
    pub end_phrase: String,
    pub submit_phrase: String,
    pub cancel_phrase: String,
    pub text_grace_ms: u64,
}

impl Default for CueConfig {
    fn default() -> Self {
        Self {
            pre_gap_ms: 250,
            confirm_ms: 800,
            inactivity_ms: 3_000,
            max_cue_ms: 2_000,
            end_phrase: "over".into(),
            submit_phrase: "out".into(),
            cancel_phrase: "cancel".into(),
            text_grace_ms: 1_800,
        }
    }
}

impl CueConfig {
    pub fn action_for(&self, text: &str) -> Option<CueAction> {
        let normalized = normalize(text);
        if normalized == normalize(&self.end_phrase) {
            Some(CueAction::End)
        } else if normalized == normalize(&self.submit_phrase) {
            Some(CueAction::EndSubmit)
        } else if normalized == normalize(&self.cancel_phrase) {
            Some(CueAction::Cancel)
        } else {
            None
        }
    }
}

pub fn normalize(text: &str) -> String {
    text.split_whitespace()
        .map(|word| {
            word.trim_matches(|c: char| c.is_ascii_punctuation())
                .to_ascii_lowercase()
        })
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

struct PendingCue {
    text: String,
    action: CueAction,
    end_ms: u64,
}

pub fn resolve(segments: &[Segment], config: &CueConfig, session_end_ms: u64) -> Resolution {
    let mut inserted = Vec::new();
    let mut events = Vec::new();
    let mut pending: Option<PendingCue> = None;
    let mut previous_end: Option<u64> = None;
    let mut last_end = segments
        .first()
        .map(|segment| segment.start_ms)
        .unwrap_or(0);

    for segment in segments {
        if let Some(candidate) = pending.take() {
            let gap = segment.start_ms.saturating_sub(candidate.end_ms);
            if gap >= config.confirm_ms {
                events.push(CueEvent::Action(candidate.action));
                return Resolution {
                    termination: Termination::Cue(candidate.action),
                    inserted,
                    events,
                };
            }
            inserted.push(candidate.text.clone());
            events.push(CueEvent::Reject(candidate.text));
        }

        let gap = previous_end
            .map(|end| segment.start_ms.saturating_sub(end))
            .unwrap_or(0);

        if previous_end.is_some() && gap >= config.inactivity_ms {
            events.push(CueEvent::Timeout);
            return Resolution {
                termination: Termination::Timeout,
                inserted,
                events,
            };
        }

        let action = config.action_for(&segment.text);
        let has_boundary = previous_end.is_none() || gap >= config.pre_gap_ms;

        if has_boundary
            && segment.duration_ms() <= config.max_cue_ms
            && let Some(action) = action
        {
            pending = Some(PendingCue {
                text: segment.text.clone(),
                action,
                end_ms: segment.end_ms,
            });
            events.push(CueEvent::Pending(action));
            previous_end = Some(segment.end_ms);
            last_end = segment.end_ms;
            continue;
        }

        inserted.push(segment.text.clone());
        events.push(CueEvent::Insert(segment.text.clone()));
        previous_end = Some(segment.end_ms);
        last_end = segment.end_ms;
    }

    let trailing = session_end_ms.saturating_sub(last_end);
    if let Some(candidate) = pending {
        if trailing >= config.confirm_ms {
            events.push(CueEvent::Action(candidate.action));
            Resolution {
                termination: Termination::Cue(candidate.action),
                inserted,
                events,
            }
        } else {
            inserted.push(candidate.text.clone());
            events.push(CueEvent::Reject(candidate.text));
            Resolution {
                termination: Termination::None,
                inserted,
                events,
            }
        }
    } else if trailing >= config.inactivity_ms {
        events.push(CueEvent::Timeout);
        Resolution {
            termination: Termination::Timeout,
            inserted,
            events,
        }
    } else {
        Resolution {
            termination: Termination::None,
            inserted,
            events,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segment(text: &str, start: u64, end: u64) -> Segment {
        Segment::new(text, start, end)
    }

    fn assert_resolution(
        segments: &[Segment],
        config: &CueConfig,
        session_end: u64,
        termination: Termination,
        inserted: &[&str],
    ) {
        let result = resolve(segments, config, session_end);
        assert_eq!(result.termination, termination);
        assert_eq!(
            result.inserted,
            inserted
                .iter()
                .map(|text| text.to_string())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn end_cue() {
        let config = CueConfig::default();
        assert_resolution(
            &[segment("hello", 0, 1_000), segment("over", 1_300, 1_700)],
            &config,
            2_600,
            Termination::Cue(CueAction::End),
            &["hello"],
        );
    }

    #[test]
    fn submit_cue() {
        let config = CueConfig::default();
        assert_resolution(
            &[segment("hello", 0, 1_000), segment("out", 1_300, 1_700)],
            &config,
            2_600,
            Termination::Cue(CueAction::EndSubmit),
            &["hello"],
        );
    }

    #[test]
    fn no_pre_gap() {
        let config = CueConfig::default();
        assert_resolution(
            &[segment("hello", 0, 1_000), segment("over", 1_100, 1_400)],
            &config,
            4_400,
            Termination::Timeout,
            &["hello", "over"],
        );
    }

    #[test]
    fn no_post_gap() {
        let config = CueConfig::default();
        assert_resolution(
            &[
                segment("hello", 0, 1_000),
                segment("over", 1_300, 1_700),
                segment("this", 2_000, 2_400),
            ],
            &config,
            5_400,
            Termination::Timeout,
            &["hello", "over", "this"],
        );
    }

    #[test]
    fn multi_word_cue_configured() {
        let config = CueConfig {
            submit_phrase: "over and out".into(),
            ..Default::default()
        };
        assert_resolution(
            &[
                segment("hello", 0, 1_000),
                segment("over and out", 1_300, 2_500),
            ],
            &config,
            3_400,
            Termination::Cue(CueAction::EndSubmit),
            &["hello"],
        );
    }

    #[test]
    fn multi_word_cue_not_configured() {
        let config = CueConfig::default();
        assert_resolution(
            &[
                segment("hello", 0, 1_000),
                segment("over and out", 1_300, 2_500),
            ],
            &config,
            5_500,
            Termination::Timeout,
            &["hello", "over and out"],
        );
    }

    #[test]
    fn cue_at_session_start() {
        let config = CueConfig::default();
        assert_resolution(
            &[segment("out", 0, 300)],
            &config,
            1_200,
            Termination::Cue(CueAction::EndSubmit),
            &[],
        );
    }

    #[test]
    fn inactivity_timeout() {
        let config = CueConfig::default();
        assert_resolution(
            &[segment("hello", 0, 1_000)],
            &config,
            4_000,
            Termination::Timeout,
            &["hello"],
        );
    }

    #[test]
    fn cue_too_long() {
        let config = CueConfig::default();
        assert_resolution(
            &[segment("hello", 0, 1_000), segment("over", 1_300, 3_400)],
            &config,
            6_400,
            Termination::Timeout,
            &["hello", "over"],
        );
    }

    #[test]
    fn ordinary_sentence_ending_in_cue_word() {
        let config = CueConfig::default();
        assert_resolution(
            &[segment("the meeting is over", 0, 1_500)],
            &config,
            4_500,
            Termination::Timeout,
            &["the meeting is over"],
        );
    }

    #[test]
    fn pre_gap_below_threshold() {
        let config = CueConfig::default();
        assert_resolution(
            &[segment("hello", 0, 1_000), segment("out", 1_100, 1_400)],
            &config,
            4_400,
            Termination::Timeout,
            &["hello", "out"],
        );
    }

    #[test]
    fn confirmation_beats_timeout() {
        let config = CueConfig::default();
        assert_resolution(
            &[segment("hello", 0, 1_000), segment("out", 1_300, 1_700)],
            &config,
            2_500,
            Termination::Cue(CueAction::EndSubmit),
            &["hello"],
        );
    }

    #[test]
    fn speech_after_confirmation_is_ignored() {
        let config = CueConfig::default();
        assert_resolution(
            &[
                segment("hello", 0, 1_000),
                segment("out", 1_300, 1_700),
                segment("later", 2_600, 3_000),
            ],
            &config,
            6_000,
            Termination::Cue(CueAction::EndSubmit),
            &["hello"],
        );
    }

    #[test]
    fn prefix_split_by_pause() {
        let config = CueConfig {
            submit_phrase: "over and out".into(),
            ..Default::default()
        };
        assert_resolution(
            &[
                segment("over", 1_300, 1_700),
                segment("and out", 2_000, 2_500),
            ],
            &config,
            5_500,
            Termination::Timeout,
            &["over", "and out"],
        );
    }

    #[test]
    fn prefix_contiguous_configured() {
        let config = CueConfig {
            submit_phrase: "over and out".into(),
            ..Default::default()
        };
        assert_resolution(
            &[segment("over and out", 1_300, 2_500)],
            &config,
            3_400,
            Termination::Cue(CueAction::EndSubmit),
            &[],
        );
    }

    #[test]
    fn cancel_cue() {
        let config = CueConfig::default();
        assert_resolution(
            &[segment("hello", 0, 1_000), segment("cancel", 1_300, 1_700)],
            &config,
            2_600,
            Termination::Cue(CueAction::Cancel),
            &["hello"],
        );
    }
}
