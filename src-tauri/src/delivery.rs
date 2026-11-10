//! Target-bound ordered text delivery and recovery.

use crate::submit::{SubmitKey, SubmitOutcome, SubmitRefusal};
use crate::target::TargetClass;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryStatus {
    Inserted,
    Failed,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextCommit {
    pub commit_id: u64,
    pub text: String,
    pub target_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryAck {
    pub commit_id: u64,
    pub status: DeliveryStatus,
}

pub trait Target {
    fn capability(&self) -> TargetClass;
    fn insert(&mut self, commit: &TextCommit) -> DeliveryStatus;
    fn submit(&mut self, key: SubmitKey) -> SubmitOutcome;
}

/// In-app transcript fallback. It always accepts text and never sends a key
/// into another application.
#[derive(Debug, Default)]
pub struct FallbackTarget {
    transcript: String,
}

impl FallbackTarget {
    pub fn transcript(&self) -> &str {
        &self.transcript
    }
}

impl Target for FallbackTarget {
    fn capability(&self) -> TargetClass {
        TargetClass::Unknown
    }

    fn insert(&mut self, commit: &TextCommit) -> DeliveryStatus {
        self.transcript.push_str(&commit.text);
        DeliveryStatus::Inserted
    }

    fn submit(&mut self, _key: SubmitKey) -> SubmitOutcome {
        SubmitOutcome::Refused(SubmitRefusal::Policy)
    }
}

/// Test target. It records commits and submit attempts, and can inject one
/// insertion failure.
#[derive(Debug)]
pub struct FakeTarget {
    capability: TargetClass,
    commits: Vec<TextCommit>,
    submits: Vec<SubmitKey>,
    fail_next: bool,
}

impl FakeTarget {
    pub fn new(capability: TargetClass) -> Self {
        Self {
            capability,
            commits: Vec::new(),
            submits: Vec::new(),
            fail_next: false,
        }
    }

    pub fn fail_next_insert(&mut self) {
        self.fail_next = true;
    }

    pub fn commits(&self) -> &[TextCommit] {
        &self.commits
    }

    pub fn submits(&self) -> &[SubmitKey] {
        &self.submits
    }
}

impl Target for FakeTarget {
    fn capability(&self) -> TargetClass {
        self.capability
    }

    fn insert(&mut self, commit: &TextCommit) -> DeliveryStatus {
        if self.fail_next {
            self.fail_next = false;
            return DeliveryStatus::Failed;
        }
        self.commits.push(commit.clone());
        DeliveryStatus::Inserted
    }

    fn submit(&mut self, key: SubmitKey) -> SubmitOutcome {
        self.submits.push(key);
        SubmitOutcome::Sent
    }
}

/// Keeps the newest recovery text, bounded by item count and 64 KiB.
#[derive(Debug, Clone)]
pub struct RecoveryBuffer {
    limit: usize,
    text: Vec<String>,
    bytes: usize,
}

impl RecoveryBuffer {
    pub fn new(limit: usize) -> Self {
        Self {
            limit,
            text: Vec::new(),
            bytes: 0,
        }
    }

    pub fn push(&mut self, value: impl Into<String>) {
        const MAX_BYTES: usize = 64 * 1024;
        if self.limit == 0 {
            return;
        }
        let mut value = value.into();
        let mut start = value.len().saturating_sub(MAX_BYTES);
        while !value.is_char_boundary(start) {
            start += 1;
        }
        if start > 0 {
            value = value.split_off(start);
        }
        while !self.text.is_empty()
            && (self.text.len() >= self.limit || self.bytes + value.len() > MAX_BYTES)
        {
            self.bytes -= self.text.remove(0).len();
        }
        self.bytes += value.len();
        self.text.push(value);
    }

    pub fn drain(&mut self) -> Vec<String> {
        self.bytes = 0;
        std::mem::take(&mut self.text)
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn len(&self) -> usize {
        self.text.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_handles_zero_capacity_and_large_unicode_values() {
        let mut disabled = RecoveryBuffer::new(0);
        disabled.push("test");
        assert!(disabled.is_empty());
        let mut buffer = RecoveryBuffer::new(100);
        buffer.push("é".repeat(100_000));
        assert!(buffer.bytes <= 64 * 1024);
        assert!(buffer.drain()[0].chars().all(|c| c == 'é'));
        assert_eq!(buffer.bytes, 0);
    }

    fn commit(id: u64, text: &str) -> TextCommit {
        TextCommit {
            commit_id: id,
            text: text.into(),
            target_id: "target".into(),
        }
    }

    #[test]
    fn recovery_buffer_is_bounded() {
        let mut buffer = RecoveryBuffer::new(2);
        buffer.push("one");
        buffer.push("two");
        buffer.push("three");
        assert_eq!(buffer.len(), 2);
        assert_eq!(buffer.drain(), vec!["two", "three"]);
        assert!(buffer.is_empty());
    }

    #[test]
    fn fallback_accepts_text_and_refuses_submit() {
        let mut target = FallbackTarget::default();
        assert_eq!(target.insert(&commit(1, "hello")), DeliveryStatus::Inserted);
        assert_eq!(target.transcript(), "hello");
        assert_eq!(
            target.submit(SubmitKey::Enter),
            SubmitOutcome::Refused(SubmitRefusal::Policy)
        );
    }

    #[test]
    fn fake_target_records_and_can_fail_once() {
        let mut target = FakeTarget::new(TargetClass::VerifiedNonShell);
        target.fail_next_insert();
        assert_eq!(target.insert(&commit(1, "a")), DeliveryStatus::Failed);
        assert_eq!(target.insert(&commit(2, "b")), DeliveryStatus::Inserted);
        assert_eq!(target.commits().len(), 1);
        assert_eq!(target.submit(SubmitKey::Enter), SubmitOutcome::Sent);
        assert_eq!(target.submits(), &[SubmitKey::Enter]);
    }
}
