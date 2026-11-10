//! Exact AT-SPI target binding. Editable fields use acknowledged direct edits;
//! opted-in terminal targets use authorized Wayland keysyms or X11 AT-SPI typing.

use std::{
    collections::{HashSet, VecDeque},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use zbus::{
    blocking::{Connection, Proxy, connection::Builder},
    zvariant::OwnedObjectPath,
};

type Object = (String, OwnedObjectPath);
#[path = "linux_focus.rs"]
mod focus;

pub fn start_focus_monitor() -> Result<(), String> {
    focus::start()
}
const ACCESSIBLE: &str = "org.a11y.atspi.Accessible";
const TEXT: &str = "org.a11y.atspi.Text";
const EDITABLE: &str = "org.a11y.atspi.EditableText";

pub struct Destination {
    connection: Connection,
    object: Object,
    expected: String,
    caret: i32,
    pid: u32,
    failed: bool,
    terminal: bool,
    keyboard_input: bool,
    keyboard: Option<Arc<super::portal::Session>>,
}

fn proxy<'a>(
    connection: &'a Connection,
    object: &'a Object,
    interface: &'a str,
) -> Result<Proxy<'a>, String> {
    // Cursor and focus checks must not use cached properties.
    let build = || -> zbus::Result<Proxy<'a>> {
        zbus::blocking::proxy::Builder::new(connection)
            .destination(object.0.as_str())?
            .path(object.1.as_str())?
            .interface(interface)?
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()
    };
    build().map_err(|e| e.to_string())
}

fn safe_role(role: &str) -> bool {
    !role.contains("password") && !role.contains("unknown")
}

fn writable(
    connection: &Connection,
    object: &Object,
    allow_terminal: bool,
) -> Result<bool, String> {
    let accessible = proxy(connection, object, ACCESSIBLE)?;
    let state: Vec<u32> = accessible
        .call("GetState", &())
        .map_err(|e| e.to_string())?;
    let role: String = accessible
        .call("GetRoleName", &())
        .map_err(|e| e.to_string())?;
    let mut terminal = role == "terminal";
    // AT-SPI states: Editable=7, Enabled=8, Focused=12, Sensitive=24.
    let required = (1 << 8) | (1 << 12) | (1 << 24) | if terminal { 0 } else { 1 << 7 };
    if state.first().copied().unwrap_or(0) & required != required {
        return Err("Destination is no longer focused and writable.".into());
    }
    let interfaces: Vec<String> = accessible
        .call("GetInterfaces", &())
        .map_err(|e| e.to_string())?;
    if !terminal && !interfaces.iter().any(|s| s == TEXT) {
        return Err("Destination does not expose editable text.".into());
    }
    if !matches!(role.as_str(), "text" | "entry" | "paragraph" | "terminal") {
        return Err("Destination is not a supported text field.".into());
    }
    let mut ancestor = object.clone();
    let deadline = Instant::now() + Duration::from_millis(500);
    for _ in 0..32 {
        if Instant::now() > deadline {
            return Err("Destination verification timed out.".into());
        }
        let parent = proxy(connection, &ancestor, ACCESSIBLE)?;
        let role: String = parent.call("GetRoleName", &()).map_err(|e| e.to_string())?;
        if !safe_role(&role) {
            return Err("Secure or unverified destination refused.".into());
        }
        terminal |= role == "terminal";
        if terminal && !allow_terminal {
            return Err(
                "Enable terminal typing in Settings before dictating into a terminal.".into(),
            );
        }
        // A stale focused node in an inactive window must not receive typing.
        if matches!(role.as_str(), "frame" | "window" | "dialog") {
            let window_state: Vec<u32> = parent.call("GetState", &()).map_err(|e| e.to_string())?;
            if window_state.first().copied().unwrap_or(0) & (1 << 1) == 0 {
                return Err("Destination window is no longer active.".into());
            }
        }
        if role == "application" {
            return Ok(terminal);
        }
        let next: Object = parent.get_property("Parent").map_err(|e| e.to_string())?;
        if next.0.is_empty() || next == ancestor {
            break;
        }
        drop(parent);
        ancestor = next;
    }
    Err("Destination ancestry could not be verified.".into())
}

/// Enable the standard accessibility interface, not the screen reader. Chromium
/// may need restarting if it was launched before toolkit accessibility was on.
pub fn enable_accessibility() -> Result<(), String> {
    let connection = Builder::session()
        .map_err(|e| e.to_string())?
        .method_timeout(Duration::from_millis(500))
        .build()
        .map_err(|e| e.to_string())?;
    let status = Proxy::new(
        &connection,
        "org.a11y.Bus",
        "/org/a11y/bus",
        "org.a11y.Status",
    )
    .map_err(|e| e.to_string())?;
    let enabled: bool = status
        .get_property("IsEnabled")
        .map_err(|e| e.to_string())?;
    if !enabled {
        status
            .set_property("IsEnabled", true)
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

impl Destination {
    pub fn capture(allow_terminal: bool) -> Result<Self, String> {
        enable_accessibility()?;
        start_focus_monitor()?;
        let session = Builder::session()
            .map_err(|e| e.to_string())?
            .method_timeout(Duration::from_millis(250))
            .build()
            .map_err(|e| e.to_string())?;
        let bus = Proxy::new(&session, "org.a11y.Bus", "/org/a11y/bus", "org.a11y.Bus")
            .map_err(|e| e.to_string())?;
        let address: String = bus.call("GetAddress", &()).map_err(|e| e.to_string())?;
        let connection = Builder::address(address.as_str())
            .map_err(|e| e.to_string())?
            .method_timeout(Duration::from_millis(250))
            .build()
            .map_err(|e| e.to_string())?;
        let root = (
            "org.a11y.atspi.Registry".to_string(),
            OwnedObjectPath::try_from("/org/a11y/atspi/accessible/root")
                .map_err(|e| e.to_string())?,
        );
        let mut queue = VecDeque::from([root]);
        let mut seen = HashSet::new();
        let mut last_refusal = None;
        let deadline = Instant::now() + Duration::from_secs(2);
        while let Some(object) = queue.pop_front() {
            if Instant::now() > deadline || seen.len() >= 512 {
                break;
            }
            if !seen.insert(object.clone()) {
                continue;
            }
            let Ok(accessible) = proxy(&connection, &object, ACCESSIBLE) else {
                continue;
            };
            let state: Vec<u32> = accessible.call("GetState", &()).unwrap_or_default();
            let role: String = accessible.call("GetRoleName", &()).unwrap_or_default();
            if matches!(role.as_str(), "frame" | "window" | "dialog")
                && state.first().copied().unwrap_or(0) & (1 << 1) == 0
            {
                continue; // Do not search every widget in inactive applications.
            }
            if state.first().is_some_and(|s| s & (1 << 12) != 0) {
                let terminal = match writable(&connection, &object, allow_terminal) {
                    Ok(terminal) => terminal,
                    Err(error) => {
                        last_refusal = Some(error);
                        continue;
                    }
                };
                let dbus = Proxy::new(
                    &connection,
                    "org.freedesktop.DBus",
                    "/org/freedesktop/DBus",
                    "org.freedesktop.DBus",
                )
                .map_err(|e| e.to_string())?;
                let pid: u32 = dbus
                    .call("GetConnectionUnixProcessID", &(object.0.as_str(),))
                    .map_err(|e| e.to_string())?;
                if pid == std::process::id() {
                    return Err(
                        "Use the global shortcut while the destination field is focused.".into(),
                    );
                }
                use std::os::unix::fs::MetadataExt;
                let owner = std::fs::metadata(format!("/proc/{pid}"))
                    .map_err(|e| e.to_string())?
                    .uid();
                let own_uid = std::fs::metadata("/proc/self")
                    .map_err(|e| e.to_string())?
                    .uid();
                if owner != own_uid {
                    return Err("Privileged or foreign-user destination refused.".into());
                }
                if terminal {
                    let keyboard = if super::portal::wayland() {
                        Some(super::portal::keyboard()?)
                    } else {
                        // X11 synthesis is refused until the user opts in; X11
                        // itself offers no permission dialog to wait for.
                        super::x11_type::keyboard_allowed()?;
                        None
                    };
                    drop(dbus);
                    drop(accessible);
                    crate::app::trace(format_args!(
                        "desktop target keyboard_input=true terminal=true"
                    ));
                    return Ok(Self {
                        connection,
                        object,
                        expected: String::new(),
                        caret: 0,
                        pid,
                        failed: false,
                        terminal: true,
                        keyboard_input: true,
                        keyboard,
                    });
                }
                // Chromium/Electron and WebKit expose readable editable Text,
                // but not AT-SPI EditableText. Use consented keyboard delivery
                // with the SAME text/caret acknowledgements, not a terminal bypass.
                let interfaces: Vec<String> = accessible
                    .call("GetInterfaces", &())
                    .map_err(|e| e.to_string())?;
                let keyboard_input = !interfaces.iter().any(|s| s == EDITABLE);
                let keyboard = if keyboard_input && super::portal::wayland() {
                    Some(super::portal::keyboard().map_err(|_| "This editor requires keyboard access. In Arxell Settings choose Grant access, approve the system dialog, then retry.".to_string())?)
                } else if keyboard_input {
                    super::x11_type::keyboard_allowed()?;
                    None
                } else {
                    None
                };
                let text = proxy(&connection, &object, TEXT)?;
                let count: i32 = text
                    .get_property("CharacterCount")
                    .map_err(|e| e.to_string())?;
                if !(0..=262_144).contains(&count) {
                    return Err("Destination is too large to verify safely.".into());
                }
                let selections: i32 = text
                    .call("GetNSelections", &())
                    .map_err(|e| e.to_string())?;
                if selections != 0 {
                    return Err("Clear the destination selection before dictating.".into());
                }
                let expected: String = text
                    .call("GetText", &(0i32, -1i32))
                    .map_err(|e| e.to_string())?;
                let caret: i32 = text
                    .get_property("CaretOffset")
                    .map_err(|e| e.to_string())?;
                if caret < 0 || caret as usize > expected.chars().count() {
                    return Err("Invalid destination cursor.".into());
                }
                drop(text);
                drop(dbus);
                drop(accessible);
                crate::app::trace(format_args!(
                    "desktop target keyboard_input={keyboard_input} terminal=false"
                ));
                return Ok(Self {
                    connection,
                    object,
                    expected,
                    caret,
                    pid,
                    failed: false,
                    terminal: false,
                    keyboard_input,
                    keyboard,
                });
            }
            let children_result = accessible.call::<_, _, Vec<Object>>("GetChildren", &());
            if let Ok(children) = children_result {
                queue.extend(
                    children
                        .into_iter()
                        .take(512usize.saturating_sub(seen.len() + queue.len())),
                );
            }
        }
        Err(last_refusal.unwrap_or_else(|| {
            "No focused editable field was exposed by the destination. Enable accessibility/keyboard access in Arxell Settings; restart the destination app if it was opened before accessibility was enabled. Transcript retained in Arxell.".into()
        }))
    }

    pub fn uses_keyboard(&self) -> bool {
        self.keyboard_input
    }

    pub fn validate(&self) -> Result<(), String> {
        self.check()
    }

    fn check(&self) -> Result<(), String> {
        if self.failed {
            return Err("Destination delivery has already stopped.".into());
        }
        let terminal = writable(&self.connection, &self.object, self.terminal)?;
        if terminal != self.terminal {
            return Err("Destination type changed.".into());
        }
        if terminal {
            return Ok(());
        }
        let text = proxy(&self.connection, &self.object, TEXT)?;
        let selections: i32 = text
            .call("GetNSelections", &())
            .map_err(|e| e.to_string())?;
        if selections != 0 {
            return Err("Destination selection changed; delivery stopped.".into());
        }
        let caret: i32 = text
            .get_property("CaretOffset")
            .map_err(|e| e.to_string())?;
        let count: i32 = text
            .get_property("CharacterCount")
            .map_err(|e| e.to_string())?;
        if count < 0 || count as usize != self.expected.chars().count() || count > 262_144 {
            return Err("Destination was edited; delivery stopped.".into());
        }
        let actual: String = text
            .call("GetText", &(0i32, -1i32))
            .map_err(|e| e.to_string())?;
        if actual != self.expected || caret != self.caret {
            return Err("Destination text or cursor changed; delivery stopped.".into());
        }
        Ok(())
    }

    fn type_text(&self, value: &str, cancel: &AtomicBool) -> Result<(), String> {
        // XTest delivers to whatever window the X server has focused; verify
        // that focus still belongs to the captured process before the chunk.
        if self.keyboard.is_none() {
            super::x11_type::verify_focus(self.pid)?;
        }
        for c in crate::desktop::keyboard_text(value).chars() {
            if cancel.load(Ordering::Acquire) {
                return Err("Dictation cancelled; already typed text was not erased.".into());
            }
            // The full ancestry/cursor check ran at the chunk boundary.
            // Recheck the exact focused node before every key without
            // traversing the entire accessibility tree per character.
            let states: Vec<u32> = proxy(&self.connection, &self.object, ACCESSIBLE)?
                .call("GetState", &())
                .map_err(|e| e.to_string())?;
            let required = (1 << 8) | (1 << 12) | (1 << 24);
            if states.first().copied().unwrap_or(0) & required != required {
                return Err("Destination focus changed during typing.".into());
            }
            #[cfg(test)]
            if let Some(result) = tests::fixture_character(c) {
                result?;
                continue;
            }
            if let Some(keyboard) = &self.keyboard {
                super::portal::type_character(keyboard, c)?;
            } else {
                // Replaces the former consent-free AT-SPI DeviceEventController
                // transport, which native testing showed delivers nothing.
                super::x11_type::type_character(c)?;
            }
        }
        Ok(())
    }

    pub fn insert(&mut self, value: &str, cancel: &AtomicBool) -> Result<(), String> {
        let result = (|| {
            self.check()?;
            if self.terminal {
                return self.type_text(value, cancel);
            }
            let normalized;
            let value = if self.keyboard_input {
                normalized = crate::desktop::keyboard_text(value);
                normalized.as_str()
            } else {
                value
            };
            if cancel.load(Ordering::Acquire) {
                return Err("Dictation cancelled.".into());
            }
            let position = self
                .expected
                .char_indices()
                .nth(self.caret as usize)
                .map_or(self.expected.len(), |(i, _)| i);
            let mut expected = self.expected.clone();
            expected.insert_str(position, value);
            if expected.chars().count() > 262_144 {
                return Err("Destination size limit reached.".into());
            }
            if self.keyboard_input {
                self.type_text(value, cancel)?;
            } else {
                let inserted: bool = proxy(&self.connection, &self.object, EDITABLE)?
                    .call("InsertText", &(self.caret, value, value.len() as i32))
                    .map_err(|e| e.to_string())?;
                if !inserted {
                    return Err("Destination refused insertion.".into());
                }
            }
            self.expected = expected;
            self.caret += value.chars().count() as i32;
            let deadline = Instant::now() + Duration::from_millis(300);
            loop {
                if self.check().is_ok() {
                    return Ok(());
                }
                if Instant::now() >= deadline {
                    return Err("Insertion could not be acknowledged. No retry or submission will be attempted.".into());
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        })();
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    pub fn activate(&mut self) -> Result<(), String> {
        if self.terminal {
            return Err("Terminal submission is disabled. Press Enter yourself.".into());
        }
        self.check()?;
        self.failed = true;
        let action = proxy(&self.connection, &self.object, "org.a11y.atspi.Action")?;
        let actions: Vec<(String, String, String)> =
            action.call("GetActions", &()).map_err(|e| e.to_string())?;
        let index = actions
            .iter()
            .position(|(name, _, _)| name == "activate")
            .ok_or("Destination has no verified activation action; no Enter was sent.")?;
        let accepted: bool = action
            .call("DoAction", &(index as i32,))
            .map_err(|e| e.to_string())?;
        if accepted {
            Ok(())
        } else {
            Err("Destination refused activation.".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    type FixtureKeyboard = Box<dyn FnMut(char) -> Result<(), String>>;
    std::thread_local! { static FIXTURE_KEYBOARD: std::cell::RefCell<Option<FixtureKeyboard>> = std::cell::RefCell::new(None); }
    pub(super) fn fixture_character(c: char) -> Option<Result<(), String>> {
        FIXTURE_KEYBOARD.with(|keyboard| keyboard.borrow_mut().as_mut().map(|send| send(c)))
    }
    #[test]
    #[ignore = "Requires the isolated GTK fixture from tests/desktop-field.py; never targets an arbitrary app"]
    fn native_delivery_to_named_fixture() {
        let token = std::env::var("ARXELL_DESKTOP_TEST_TOKEN").expect("Run tests/desktop-field.py");
        let stage = std::path::PathBuf::from(
            std::env::var_os("ARXELL_DESKTOP_TEST_STAGE").expect("fixture stage file"),
        );
        enable_accessibility().expect("enable toolkit accessibility");
        start_focus_monitor().expect("accessibility focus listener");
        if std::env::var("ARXELL_DESKTOP_TEST_PORTAL_CONSENT")
            .ok()
            .as_deref()
            == Some(token.as_str())
        {
            assert!(super::super::portal::wayland());
            super::super::portal::prepare_keyboard()
                .expect("Approve Arxell keyboard access for isolated fixture");
        }
        if std::env::var_os("ARXELL_DESKTOP_TEST_ELECTRON").is_some() {
            std::fs::write(&stage, "monitor-ready").unwrap();
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut last_error = String::new();
        let mut target = loop {
            let captured = Destination::capture(false);
            if let Err(error) = &captured {
                last_error = error.clone();
            }
            if let Ok(target) = captured {
                let name: String = proxy(&target.connection, &target.object, ACCESSIBLE)
                    .unwrap()
                    .get_property("Name")
                    .unwrap_or_default();
                if name == token {
                    break target;
                }
            }
            assert!(
                Instant::now() < deadline,
                "The isolated fixture did not receive focus; no text was sent to any other target: {last_error}"
            );
            std::thread::sleep(Duration::from_millis(50));
        };
        assert!(target.expected.is_empty(), "Fixture must start empty");
        if let Some(input) = std::env::var_os("ARXELL_DESKTOP_TEST_RENDERER_INPUT") {
            assert!(
                target.keyboard_input && !target.terminal,
                "Renderer seam is only for a verified nonce-named editable Electron field"
            );
            let input = std::path::PathBuf::from(input);
            FIXTURE_KEYBOARD.with(|keyboard| {
                *keyboard.borrow_mut() = Some(Box::new(move |c| {
                    let ack = input.with_extension("ack");
                    let _ = std::fs::remove_file(&ack);
                    std::fs::write(&input, c.to_string()).map_err(|e| e.to_string())?;
                    let deadline = Instant::now() + Duration::from_secs(2);
                    while !ack.exists() {
                        if Instant::now() >= deadline {
                            return Err(
                                "Isolated renderer did not acknowledge fixture input".into()
                            );
                        }
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Ok(())
                }))
            });
        }
        let cancel = AtomicBool::new(false);
        target
            .insert("Héllo 世界", &cancel)
            .expect("first native Unicode insert");
        target
            .insert(" + streaming", &cancel)
            .expect("second native incremental insert");
        target.validate().expect("acknowledged field and caret");
        std::fs::write(&stage, "typed").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while std::fs::read_to_string(&stage).unwrap_or_default() != "focus-changed" {
            assert!(Instant::now() < deadline, "fixture did not move focus");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(target.validate().is_err());
        assert!(target.insert("MUST NOT BE TYPED", &cancel).is_err());
    }

    #[test]
    #[ignore = "Real synthetic X11/XTest keyboard input; run only with the isolated fixture from tests/desktop-electron.py in ARXELL_DESKTOP_TEST_X11_XTEST=1 mode"]
    fn xtest_delivery_to_x11_fixture() {
        let token =
            std::env::var("ARXELL_DESKTOP_TEST_TOKEN").expect("Run tests/desktop-electron.py");
        let stage = std::path::PathBuf::from(
            std::env::var_os("ARXELL_DESKTOP_TEST_STAGE").expect("fixture stage file"),
        );
        assert!(
            !super::super::portal::wayland(),
            "This test exercises the X11 transport and must run without WAYLAND_DISPLAY"
        );
        // The opt-in setting is the only consent X11 can express.
        super::super::x11_type::set_consent(true);
        enable_accessibility().expect("enable toolkit accessibility");
        start_focus_monitor().expect("accessibility focus listener");
        std::fs::write(&stage, "monitor-ready").unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut last_error = String::new();
        let mut target = loop {
            let captured = Destination::capture(false);
            if let Err(error) = &captured {
                last_error = error.clone();
            }
            if let Ok(target) = captured {
                let name: String = proxy(&target.connection, &target.object, ACCESSIBLE)
                    .unwrap()
                    .get_property("Name")
                    .unwrap_or_default();
                if name == token {
                    break target;
                }
            }
            assert!(
                Instant::now() < deadline,
                "The isolated X11 fixture did not receive focus; nothing was typed elsewhere: {last_error}"
            );
            std::thread::sleep(Duration::from_millis(50));
        };
        assert!(target.expected.is_empty(), "Fixture must start empty");
        assert!(
            target.keyboard_input && !target.terminal && target.keyboard.is_none(),
            "expected a Text-only X11 editor served by the XTest transport"
        );
        let cancel = AtomicBool::new(false);
        target
            .insert("Héllo 世界", &cancel)
            .expect("real XTest Unicode insert");
        target
            .insert(" + streaming", &cancel)
            .expect("real XTest incremental insert");
        target.validate().expect("acknowledged field and caret");
        std::fs::write(&stage, "typed").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while std::fs::read_to_string(&stage).unwrap_or_default() != "focus-changed" {
            assert!(Instant::now() < deadline, "fixture did not move focus");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(target.insert("MUST NOT BE TYPED", &cancel).is_err());
        let result =
            std::fs::read_to_string(stage.with_extension("result")).expect("fixture result");
        assert!(result.contains("Héllo 世界 + streaming"), "{result}");
    }

    #[test]
    fn refuses_sensitive_roles_and_allows_explicitly_gated_terminals() {
        for role in ["password text", "unknown"] {
            assert!(!safe_role(role));
        }
        assert!(safe_role("entry"));
        assert!(safe_role("terminal"));
    }
}
