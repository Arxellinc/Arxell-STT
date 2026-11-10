//! Dictation session state machine.
//!
//! Pure transition rules; the live worker owns capture and finalization.

use serde::Serialize;

use crate::cue::CueAction;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum SessionState {
    #[default]
    NotReady,
    Ready,
    Listening,
    CuePending,
    Finalizing,
    Delivering,
    Complete,
    Cancelled,
    RecoverableError,
}

impl SessionState {
    pub fn is_active(self) -> bool {
        matches!(
            self,
            Self::Listening | Self::CuePending | Self::Finalizing | Self::Delivering
        )
    }

    pub fn can_transition_to(self, next: Self) -> bool {
        use SessionState::*;
        match (self, next) {
            (NotReady, Ready)
            | (Ready, Listening)
            | (Listening, CuePending)
            | (CuePending, Listening)
            | (CuePending, Finalizing)
            | (Listening, Finalizing)
            | (Finalizing, Delivering)
            | (Delivering, Complete)
            | (Complete, Ready)
            | (Cancelled, Ready)
            | (RecoverableError, Ready)
            | (RecoverableError, NotReady) => true,
            (current, Cancelled) => current.is_active(),
            (current, RecoverableError) => current.is_active(),
            _ => false,
        }
    }

    pub fn after_cue(self, action: CueAction) -> Option<Self> {
        if self != Self::CuePending {
            return None;
        }
        match action {
            CueAction::End | CueAction::EndSubmit => Some(Self::Finalizing),
            CueAction::Cancel => Some(Self::Cancelled),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legal_edges() {
        use SessionState::*;
        assert!(NotReady.can_transition_to(Ready));
        assert!(Ready.can_transition_to(Listening));
        assert!(Listening.can_transition_to(CuePending));
        assert!(CuePending.can_transition_to(Listening));
        assert!(CuePending.can_transition_to(Finalizing));
        assert!(Finalizing.can_transition_to(Delivering));
        assert!(Delivering.can_transition_to(Complete));
        assert!(Complete.can_transition_to(Ready));
    }

    #[test]
    fn illegal_edges() {
        use SessionState::*;
        assert!(!Ready.can_transition_to(Delivering));
        assert!(!Listening.can_transition_to(Complete));
        assert!(!Complete.can_transition_to(Listening));
        assert!(!NotReady.can_transition_to(Listening));
    }

    #[test]
    fn active_states_can_cancel_or_fail() {
        use SessionState::*;
        for state in [Listening, CuePending, Finalizing, Delivering] {
            assert!(state.can_transition_to(Cancelled));
            assert!(state.can_transition_to(RecoverableError));
        }
        assert!(!Ready.can_transition_to(Cancelled));
    }

    #[test]
    fn cue_resolution() {
        use SessionState::*;
        assert_eq!(CuePending.after_cue(CueAction::End), Some(Finalizing));
        assert_eq!(CuePending.after_cue(CueAction::EndSubmit), Some(Finalizing));
        assert_eq!(CuePending.after_cue(CueAction::Cancel), Some(Cancelled));
        assert_eq!(Listening.after_cue(CueAction::End), None);
    }
}
