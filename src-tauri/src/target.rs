//! Target identity, capability and submission policy.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetClass {
    VerifiedNonShell,
    VerifiedAgent,
    VerifiedShell,
    Unknown,
    Secure,
    Elevated,
    Unavailable,
}

impl TargetClass {
    pub fn may_submit(self, shell_opt_in: bool) -> bool {
        match self {
            Self::VerifiedNonShell | Self::VerifiedAgent => true,
            Self::VerifiedShell => shell_opt_in,
            Self::Unknown | Self::Secure | Self::Elevated | Self::Unavailable => false,
        }
    }

    pub fn may_insert(self, allow_unknown: bool) -> bool {
        match self {
            Self::VerifiedNonShell | Self::VerifiedAgent | Self::VerifiedShell => true,
            Self::Unknown => allow_unknown,
            Self::Secure | Self::Elevated | Self::Unavailable => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn submission_policy() {
        assert!(TargetClass::VerifiedNonShell.may_submit(false));
        assert!(TargetClass::VerifiedAgent.may_submit(false));
        assert!(!TargetClass::VerifiedShell.may_submit(false));
        assert!(TargetClass::VerifiedShell.may_submit(true));
        assert!(!TargetClass::Unknown.may_submit(true));
        assert!(!TargetClass::Secure.may_submit(true));
        assert!(!TargetClass::Elevated.may_submit(true));
        assert!(!TargetClass::Unavailable.may_submit(true));
    }

    #[test]
    fn insertion_policy() {
        assert!(TargetClass::VerifiedNonShell.may_insert(false));
        assert!(TargetClass::VerifiedShell.may_insert(false));
        assert!(!TargetClass::Unknown.may_insert(false));
        assert!(TargetClass::Unknown.may_insert(true));
        assert!(!TargetClass::Secure.may_insert(true));
        assert!(!TargetClass::Elevated.may_insert(true));
        assert!(!TargetClass::Unavailable.may_insert(true));
    }
}
