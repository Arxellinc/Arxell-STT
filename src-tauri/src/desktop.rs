//! Interactive desktop delivery only. The local API cannot create this worker.
//!
//! Recognition publishes cumulative STABLE text into a single latest-value slot.
//! Focus discovery and slow accessibility calls never block capture/inference.
//! Ambiguous delivery is never retried, and a bound destination is never replaced.
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};

#[derive(Default)]
struct Pending {
    text: String,
    finished: bool,
    cancel: bool,
}

pub struct DesktopStream {
    shared: Arc<(Mutex<Pending>, Condvar)>,
    thread: Option<std::thread::JoinHandle<()>>,
}

/// Append-only delivery ledger, shared by all platforms. An unacknowledged
/// insertion is fatal: replaying it could duplicate text or a shell command.
#[derive(Default)]
pub(crate) struct AppendLedger {
    sent: String,
    failed: bool,
}
impl AppendLedger {
    pub fn suffix<'a>(&self, desired: &'a str) -> Result<&'a str, String> {
        if self.failed {
            return Err("Desktop typing has stopped for this session.".into());
        }
        desired.strip_prefix(&self.sent).ok_or_else(|| "Recognition revised text already typed. Desktop typing stopped; the full corrected transcript is in Arxell.".into())
    }
    pub fn acknowledge(&mut self, text: &str) {
        self.sent.push_str(text);
    }
    pub fn fail(&mut self) {
        self.failed = true;
    }
}

/// Keyboard paths may never turn recognized text into Enter, Escape, Tab,
/// control sequences, or terminal paste delimiters. No clipboard is involved.
pub(crate) fn keyboard_text(text: &str) -> String {
    text.chars()
        .filter_map(|c| match c {
            '\n' | '\r' | '\t' | '\u{2028}' | '\u{2029}' => Some(' '),
            c if c.is_control() => None,
            c => Some(c),
        })
        .collect()
}

/// Acknowledges one exact insertion without assuming the caret's byte offset.
/// Used only when an accessibility provider exposes the complete field value.
#[cfg(any(target_os = "windows", target_os = "macos", test))]
pub(crate) fn is_insertion(before: &str, after: &str, inserted: &str) -> bool {
    if before.len().checked_add(inserted.len()) != Some(after.len()) {
        return false;
    }
    let prefix: usize = before
        .chars()
        .zip(after.chars())
        .take_while(|(a, b)| a == b)
        .map(|(c, _)| c.len_utf8())
        .sum();
    let suffix: usize = before
        .chars()
        .rev()
        .zip(after.chars().rev())
        .take_while(|(a, b)| a == b)
        .map(|(c, _)| c.len_utf8())
        .sum();
    let start = before.len().saturating_sub(suffix);
    if start > prefix {
        return false;
    }
    before
        .char_indices()
        .map(|(i, _)| i)
        .chain(std::iter::once(before.len()))
        .filter(|i| *i >= start && *i <= prefix)
        .any(|i| after[i..].strip_prefix(inserted) == Some(&before[i..]))
}

/// Retain a possible spoken-control suffix until it is confirmed as ordinary
/// dictation or stripped by cue resolution. Never type a tentative hypothesis.
pub(crate) fn safe_committed<'a>(text: &'a str, cues: Option<&crate::cue::CueConfig>) -> &'a str {
    let text = text.trim_end();
    let Some(cues) = cues else {
        return text;
    };
    let words = [&cues.end_phrase, &cues.submit_phrase, &cues.cancel_phrase]
        .iter()
        .map(|p| crate::cue::normalize(p).split_whitespace().count())
        .max()
        .unwrap_or(1);
    let starts = text
        .char_indices()
        .rev()
        .filter(|(i, c)| {
            !c.is_whitespace() && (*i == 0 || text[..*i].ends_with(char::is_whitespace))
        })
        .take(words);
    for (index, _) in starts {
        let suffix = crate::cue::normalize(&text[index..]);
        if !suffix.is_empty()
            && [&cues.end_phrase, &cues.submit_phrase, &cues.cancel_phrase]
                .iter()
                .any(|phrase| crate::cue::normalize(phrase).starts_with(&suffix))
        {
            return text[..index].trim_end();
        }
    }
    text
}

impl DesktopStream {
    pub fn start(
        app: AppHandle,
        cancel: Arc<AtomicBool>,
        wait_for_focus: bool,
        terminals: bool,
    ) -> Result<Self, String> {
        let shared = Arc::new((Mutex::new(Pending::default()), Condvar::new()));
        let worker = shared.clone();
        let thread = std::thread::Builder::new()
            .name("desktop-delivery".into())
            .spawn(move || {
                let warn = |message| {
                    crate::live::emit(
                        &app,
                        crate::live::SessionEvent::Delivery {
                            state: "stopped".into(),
                            message,
                        },
                    )
                };
                let result = run(&app, &worker, &cancel, wait_for_focus, terminals);
                if let Err(message) = result {
                    warn(message);
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            shared,
            thread: Some(thread),
        })
    }
    pub fn publish(&self, text: &str) {
        let (lock, ready) = &*self.shared;
        if let Ok(mut pending) = lock.lock() {
            if pending.finished || pending.cancel {
                return;
            }
            pending.text = text.to_owned();
            ready.notify_one();
        }
    }
    pub fn finish(&mut self, text: &str) {
        let (lock, ready) = &*self.shared;
        if let Ok(mut pending) = lock.lock() {
            pending.text = text.to_owned();
            pending.finished = true;
            ready.notify_one();
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
impl Drop for DesktopStream {
    fn drop(&mut self) {
        if self.thread.is_none() {
            return;
        }
        let (lock, ready) = &*self.shared;
        if let Ok(mut pending) = lock.lock() {
            pending.cancel = true;
            ready.notify_one();
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run(
    app: &AppHandle,
    shared: &(Mutex<Pending>, Condvar),
    cancel: &AtomicBool,
    wait_for_focus: bool,
    terminals: bool,
) -> Result<(), String> {
    run_delivery(
        shared,
        cancel,
        || crate::platform::Destination::capture(terminals),
        || {
            app.state::<crate::app::AppState>()
                .shortcut_held
                .load(Ordering::Acquire)
                || crate::platform::modifiers_down()
        },
        |event| crate::live::emit(app, event),
        if wait_for_focus {
            Duration::from_secs(15)
        } else {
            Duration::from_secs(3)
        },
    )
}

trait TextTarget {
    fn uses_keyboard(&self) -> bool {
        true
    }
    fn validate(&self) -> Result<(), String>;
    fn insert(&mut self, text: &str, cancel: &AtomicBool) -> Result<(), String>;
}
impl TextTarget for crate::platform::Destination {
    fn uses_keyboard(&self) -> bool {
        #[cfg(target_os = "linux")]
        {
            self.uses_keyboard()
        }
        #[cfg(not(target_os = "linux"))]
        {
            true
        }
    }
    fn validate(&self) -> Result<(), String> {
        self.validate()
    }
    fn insert(&mut self, text: &str, cancel: &AtomicBool) -> Result<(), String> {
        self.insert(text, cancel)
    }
}

fn run_delivery<T: TextTarget>(
    shared: &(Mutex<Pending>, Condvar),
    cancel: &AtomicBool,
    mut capture: impl FnMut() -> Result<T, String>,
    blocked: impl Fn() -> bool,
    notify: impl Fn(crate::live::SessionEvent),
    deadline: Duration,
) -> Result<(), String> {
    let started = Instant::now();
    /* Focus acquisition has a deadline, not an unlimited background retarget. */
    let mut destination = None;
    let mut ledger = AppendLedger::default();
    let mut last_capture_error = String::new();
    let mut modifiers_since: Option<Instant> = None;
    notify(crate::live::SessionEvent::Delivery {
        state: "waiting".into(),
        message:
            "Waiting for an external text field. Click the destination to lock desktop typing."
                .into(),
    });
    loop {
        let (text, finished) = {
            let mut pending = shared.0.lock().map_err(|_| "Desktop queue failed")?;
            if pending.cancel || cancel.load(Ordering::Acquire) {
                return Ok(());
            }
            if pending.text == ledger.sent && !pending.finished {
                pending = shared
                    .1
                    .wait_timeout(pending, Duration::from_millis(60))
                    .map_err(|_| "Desktop queue failed")?
                    .0;
                if pending.cancel || cancel.load(Ordering::Acquire) {
                    return Ok(());
                }
            }
            (pending.text.clone(), pending.finished)
        };
        if finished && text.is_empty() {
            return Ok(());
        }
        if destination.is_none() {
            match capture() {
                Ok(target) => {
                    destination = Some(target);
                    notify(crate::live::SessionEvent::Delivery { state: "bound".into(), message: "Desktop target locked. Moving focus stops typing; text is always retained in Arxell.".into() });
                }
                Err(message) => last_capture_error = message,
            }
            if destination.is_none() {
                if started.elapsed() >= deadline || finished {
                    return Err(format!(
                        "Desktop target unavailable: {last_capture_error} Transcript retained in Arxell."
                    ));
                }
                std::thread::sleep(Duration::from_millis(60));
                continue;
            }
        }
        let target = destination.as_mut().ok_or("Desktop target missing")?;
        target.validate().map_err(|error| {
            format!("Desktop typing stopped: {error} Transcript retained in Arxell.")
        })?;
        // Direct accessibility edits do not synthesize keys. A missing release
        // signal must not block them; keep the guard for actual keyboard input.
        if target.uses_keyboard() && blocked() {
            let since = modifiers_since.get_or_insert_with(Instant::now);
            if finished && since.elapsed() > Duration::from_secs(2) {
                return Err(
                    "Shortcut/key release could not be verified. Keyboard delivery stopped; remaining text was kept in Arxell.".into(),
                );
            }
            std::thread::sleep(Duration::from_millis(30));
            continue;
        }
        modifiers_since = None;
        let suffix: String = ledger.suffix(&text)?.chars().take(64).collect();
        if !suffix.is_empty() {
            if cancel.load(Ordering::Acquire) {
                return Ok(());
            }
            if let Err(error) = target.insert(&suffix, cancel) {
                ledger.fail();
                return Err(format!(
                    "Desktop typing stopped: {error} No retry was sent. Transcript retained in Arxell."
                ));
            }
            ledger.acknowledge(&suffix);
        }
        if finished && ledger.sent == text {
            notify(crate::live::SessionEvent::Delivery {
                state: "complete".into(),
                message: "Desktop typing complete. No Enter or submit key was sent.".into(),
            });
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[derive(Default)]
    struct MockState {
        text: String,
        inserts: usize,
        uncertain: bool,
    }
    struct MockTarget {
        state: Arc<Mutex<MockState>>,
        focused: Arc<AtomicBool>,
        uses_keyboard: bool,
    }
    impl TextTarget for MockTarget {
        fn uses_keyboard(&self) -> bool {
            self.uses_keyboard
        }
        fn validate(&self) -> Result<(), String> {
            if self.focused.load(Ordering::Acquire) {
                Ok(())
            } else {
                Err("focus changed".into())
            }
        }
        fn insert(&mut self, text: &str, _: &AtomicBool) -> Result<(), String> {
            self.validate()?;
            let mut state = self.state.lock().unwrap();
            state.text.push_str(text);
            state.inserts += 1;
            if state.uncertain {
                Err("acknowledgement lost".into())
            } else {
                Ok(())
            }
        }
    }
    struct Harness {
        shared: Arc<(Mutex<Pending>, Condvar)>,
        cancel: Arc<AtomicBool>,
        focused: Arc<AtomicBool>,
        blocked: Arc<AtomicBool>,
        state: Arc<Mutex<MockState>>,
        captures: Arc<AtomicUsize>,
        bound: std::sync::mpsc::Receiver<()>,
        thread: std::thread::JoinHandle<Result<(), String>>,
    }
    impl Harness {
        fn new(late_binding: bool, uncertain: bool) -> Self {
            Self::with_transport(late_binding, uncertain, true)
        }
        fn with_transport(late_binding: bool, uncertain: bool, uses_keyboard: bool) -> Self {
            let shared = Arc::new((Mutex::new(Pending::default()), Condvar::new()));
            let cancel = Arc::new(AtomicBool::new(false));
            let focused = Arc::new(AtomicBool::new(true));
            let blocked = Arc::new(AtomicBool::new(false));
            let state = Arc::new(Mutex::new(MockState {
                uncertain,
                ..Default::default()
            }));
            let captures = Arc::new(AtomicUsize::new(0));
            let (bound_tx, bound) = std::sync::mpsc::channel();
            let (s, c, f, b, m, n) = (
                shared.clone(),
                cancel.clone(),
                focused.clone(),
                blocked.clone(),
                state.clone(),
                captures.clone(),
            );
            let thread = std::thread::spawn(move || {
                run_delivery(
                    &s,
                    &c,
                    || {
                        let attempt = n.fetch_add(1, Ordering::AcqRel);
                        if late_binding && attempt < 2 {
                            return Err("user is still choosing a field".into());
                        }
                        Ok(MockTarget {
                            state: m.clone(),
                            focused: f.clone(),
                            uses_keyboard,
                        })
                    },
                    || b.load(Ordering::Acquire),
                    |event| {
                        if matches!(event, crate::live::SessionEvent::Delivery { state, .. } if state == "bound")
                        {
                            let _ = bound_tx.send(());
                        }
                    },
                    Duration::from_secs(2),
                )
            });
            Self {
                shared,
                cancel,
                focused,
                blocked,
                state,
                captures,
                bound,
                thread,
            }
        }
        fn publish(&self, text: &str, finished: bool) {
            let mut pending = self.shared.0.lock().unwrap();
            pending.text = text.to_owned();
            pending.finished = finished;
            self.shared.1.notify_one();
        }
        fn wait_bound(&self) {
            self.bound.recv_timeout(Duration::from_secs(2)).unwrap();
        }
        fn wait_text(&self, text: &str) {
            let deadline = Instant::now() + Duration::from_secs(2);
            while self.state.lock().unwrap().text != text {
                assert!(Instant::now() < deadline, "text was not delivered");
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
    #[test]
    fn worker_streams_before_finish_and_delivers_only_new_suffixes() {
        let h = Harness::new(false, false);
        h.wait_bound();
        h.publish("Héllo", false);
        h.wait_text("Héllo");
        h.publish("Héllo", false);
        h.publish("Héllo 世界", true);
        h.thread.join().unwrap().unwrap();
        assert_eq!(h.state.lock().unwrap().text, "Héllo 世界");
        assert_eq!(h.captures.load(Ordering::Acquire), 1);
    }
    #[test]
    fn worker_late_binds_after_microphone_click() {
        let h = Harness::new(true, false);
        h.publish("queued words", false);
        h.wait_bound();
        h.wait_text("queued words");
        h.publish("queued words", true);
        h.thread.join().unwrap().unwrap();
        assert_eq!(h.captures.load(Ordering::Acquire), 3);
    }
    #[test]
    fn worker_stops_on_focus_loss_even_between_recognition_updates() {
        let h = Harness::new(false, false);
        h.wait_bound();
        h.focused.store(false, Ordering::Release);
        assert!(h.thread.join().unwrap().is_err());
        assert!(h.state.lock().unwrap().text.is_empty());
        assert_eq!(h.captures.load(Ordering::Acquire), 1);
    }
    #[test]
    fn worker_never_retries_a_partially_acknowledged_insert() {
        let h = Harness::new(false, true);
        h.wait_bound();
        h.publish("hello", false);
        h.wait_text("hello");
        h.publish("hello world", true);
        assert!(h.thread.join().unwrap().is_err());
        assert_eq!(h.state.lock().unwrap().inserts, 1);
    }
    #[test]
    fn worker_cancellation_and_revisions_never_erase_existing_text() {
        let h = Harness::new(false, false);
        h.wait_bound();
        h.publish("original", false);
        h.wait_text("original");
        h.publish("revised", true);
        assert!(h.thread.join().unwrap().is_err());
        assert_eq!(h.state.lock().unwrap().text, "original");
        let h = Harness::new(false, false);
        h.wait_bound();
        h.cancel.store(true, Ordering::Release);
        h.publish("not typed", true);
        h.thread.join().unwrap().unwrap();
        assert!(h.state.lock().unwrap().text.is_empty());
    }
    #[test]
    fn direct_edit_does_not_wait_for_a_missing_shortcut_release() {
        let h = Harness::with_transport(false, false, false);
        h.wait_bound();
        h.blocked.store(true, Ordering::Release);
        h.publish("direct edit", true);
        h.thread.join().unwrap().unwrap();
        assert_eq!(h.state.lock().unwrap().text, "direct edit");
    }
    #[test]
    fn worker_waits_until_activation_modifiers_are_released() {
        let h = Harness::new(false, false);
        h.wait_bound();
        h.blocked.store(true, Ordering::Release);
        h.publish("hello", false);
        std::thread::sleep(Duration::from_millis(90));
        assert!(h.state.lock().unwrap().text.is_empty());
        h.blocked.store(false, Ordering::Release);
        h.wait_text("hello");
        h.publish("hello", true);
        h.thread.join().unwrap().unwrap();
    }
    #[test]
    fn worker_batches_large_unicode_updates_without_truncation() {
        let h = Harness::new(false, false);
        h.wait_bound();
        let text = "héllo 世界 ".repeat(50);
        h.publish(&text, true);
        h.thread.join().unwrap().unwrap();
        assert_eq!(h.state.lock().unwrap().text, text);
        assert!(h.state.lock().unwrap().inserts > 1);
    }

    #[test]
    fn verifies_exact_insertions_without_erasures() {
        assert!(is_insertion("abc", "aXabc", "aX"));
        assert!(is_insertion("aaa", "abaaa", "ba"));
        assert!(is_insertion("héllo", "hé世界llo", "世界"));
        assert!(!is_insertion("hello", "HELLO world", " world"));
        assert!(!is_insertion("abc", "abX", "X"));
    }
    #[test]
    fn spoken_control_suffixes_are_never_typed_early() {
        let cue = crate::cue::CueConfig::default();
        assert_eq!(
            safe_committed("Please check the build over", Some(&cue)),
            "Please check the build"
        );
        assert_eq!(safe_committed("over", Some(&cue)), "");
        assert_eq!(
            safe_committed("Look over this code", Some(&cue)),
            "Look over this code"
        );
        assert_eq!(safe_committed("ordinary text", Some(&cue)), "ordinary text");
        assert_eq!(safe_committed("over", None), "over");
        let mut cue = cue;
        cue.end_phrase = "finish dictation".into();
        assert_eq!(safe_committed("hello finish  dicta", Some(&cue)), "hello");
        assert_eq!(
            safe_committed("hello finish dictation", Some(&cue)),
            "hello"
        );
    }
    #[test]
    fn suffixes_are_unicode_safe_and_never_duplicate() {
        let mut ledger = AppendLedger::default();
        assert_eq!(ledger.suffix("Héllo").unwrap(), "Héllo");
        ledger.acknowledge("Héllo");
        assert_eq!(ledger.suffix("Héllo").unwrap(), "");
        assert_eq!(ledger.suffix("Héllo 世界").unwrap(), " 世界");
        assert!(ledger.suffix("Hello 世界").is_err());
    }
    #[test]
    fn uncertain_insert_is_not_retried() {
        let mut ledger = AppendLedger::default();
        ledger.fail();
        assert!(ledger.suffix("hello").is_err());
    }
    #[test]
    fn terminal_text_cannot_send_control_keys_or_escape_sequences() {
        assert_eq!(
            keyboard_text("hello\nworld\r\t\x1b[200~\0\x7f\u{85}"),
            "hello world  [200~"
        );
        assert_eq!(keyboard_text("héllo 世界"), "héllo 世界");
    }
}
