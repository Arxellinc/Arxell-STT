//! Guarded submit-key dispatch. Exactly once, no retry after uncertainty.

use serde::Serialize;

use crate::target::TargetClass;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
pub enum SubmitKey {
    Enter,
    CtrlEnter,
    CmdEnter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum SubmitOutcome {
    Sent,
    Refused(SubmitRefusal),
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum SubmitRefusal {
    AlreadySent,
    NoResolvedCue,
    DivergentFinal,
    SecureTarget,
    TargetChanged,
    DeliveryNotAcked,
    Policy,
}

#[derive(Debug, Clone, Copy)]
pub struct SubmitContext {
    pub cue_resolved: bool,
    pub delivery_acked: bool,
    pub target_matches: bool,
    pub target: TargetClass,
    pub shell_opt_in: bool,
    pub final_diverged: bool,
    pub already_sent: bool,
    pub key: SubmitKey,
}

/// Policy check; dispatch must also use the per-session guard.
pub fn authorize(context: SubmitContext) -> Result<SubmitKey, SubmitRefusal> {
    if context.already_sent {
        return Err(SubmitRefusal::AlreadySent);
    }
    if !context.cue_resolved {
        return Err(SubmitRefusal::NoResolvedCue);
    }
    if context.final_diverged {
        return Err(SubmitRefusal::DivergentFinal);
    }
    if matches!(context.target, TargetClass::Secure | TargetClass::Elevated) {
        return Err(SubmitRefusal::SecureTarget);
    }
    if !context.target_matches {
        return Err(SubmitRefusal::TargetChanged);
    }
    if !context.delivery_acked {
        return Err(SubmitRefusal::DeliveryNotAcked);
    }
    if !context.target.may_submit(context.shell_opt_in) {
        return Err(SubmitRefusal::Policy);
    }
    Ok(context.key)
}

#[derive(Default)]
pub struct SubmitGuard {
    attempted: bool,
}

impl SubmitGuard {
    pub fn dispatch(
        &mut self,
        mut context: SubmitContext,
        send: impl FnOnce(SubmitKey) -> SubmitOutcome,
    ) -> SubmitOutcome {
        context.already_sent |= self.attempted;
        match authorize(context) {
            Ok(key) => {
                self.attempted = true;
                send(key)
            }
            Err(reason) => SubmitOutcome::Refused(reason),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uncertain_dispatch_cannot_be_retried_by_resetting_caller_flags() {
        let mut guard = SubmitGuard::default();
        assert_eq!(
            guard.dispatch(context(), |_| SubmitOutcome::Unknown),
            SubmitOutcome::Unknown
        );
        assert_eq!(
            guard.dispatch(context(), |_| panic!("must not dispatch twice")),
            SubmitOutcome::Refused(SubmitRefusal::AlreadySent)
        );
    }

    fn context() -> SubmitContext {
        SubmitContext {
            cue_resolved: true,
            delivery_acked: true,
            target_matches: true,
            target: TargetClass::VerifiedNonShell,
            shell_opt_in: false,
            final_diverged: false,
            already_sent: false,
            key: SubmitKey::Enter,
        }
    }

    #[test]
    fn allows_verified_non_shell() {
        assert_eq!(authorize(context()), Ok(SubmitKey::Enter));
    }

    #[test]
    fn refuses_without_cue() {
        let mut ctx = context();
        ctx.cue_resolved = false;
        assert_eq!(authorize(ctx), Err(SubmitRefusal::NoResolvedCue));
    }

    #[test]
    fn refuses_unacked_delivery() {
        let mut ctx = context();
        ctx.delivery_acked = false;
        assert_eq!(authorize(ctx), Err(SubmitRefusal::DeliveryNotAcked));
    }

    #[test]
    fn refuses_target_change() {
        let mut ctx = context();
        ctx.target_matches = false;
        assert_eq!(authorize(ctx), Err(SubmitRefusal::TargetChanged));
    }

    #[test]
    fn refuses_shell_without_opt_in() {
        let mut ctx = context();
        ctx.target = TargetClass::VerifiedShell;
        assert_eq!(authorize(ctx), Err(SubmitRefusal::Policy));
        ctx.shell_opt_in = true;
        assert_eq!(authorize(ctx), Ok(SubmitKey::Enter));
    }

    #[test]
    fn refuses_unknown_target() {
        let mut ctx = context();
        ctx.target = TargetClass::Unknown;
        assert_eq!(authorize(ctx), Err(SubmitRefusal::Policy));
    }

    #[test]
    fn refuses_secure_target() {
        let mut ctx = context();
        ctx.target = TargetClass::Secure;
        assert_eq!(authorize(ctx), Err(SubmitRefusal::SecureTarget));
    }

    #[test]
    fn refuses_divergent_final() {
        let mut ctx = context();
        ctx.final_diverged = true;
        assert_eq!(authorize(ctx), Err(SubmitRefusal::DivergentFinal));
    }

    #[test]
    fn refuses_double_send() {
        let mut ctx = context();
        ctx.already_sent = true;
        assert_eq!(authorize(ctx), Err(SubmitRefusal::AlreadySent));
    }
}
