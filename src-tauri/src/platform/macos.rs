//! macOS Accessibility binding and targeted Unicode CGEvents. No clipboard or
//! Return key. Requires Accessibility consent and real macOS validation.
use std::{
    ffi::{c_char, c_void},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
type Ref = *const c_void;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
struct Range {
    location: isize,
    length: isize,
}
#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXIsProcessTrustedWithOptions(options: Ref) -> bool;
    static kAXTrustedCheckOptionPrompt: Ref;
    fn AXUIElementCreateSystemWide() -> Ref;
    fn AXUIElementCopyAttributeValue(element: Ref, attribute: Ref, value: *mut Ref) -> i32;
    fn AXUIElementGetPid(element: Ref, pid: *mut i32) -> i32;
    fn AXUIElementSetMessagingTimeout(element: Ref, timeout: f32) -> i32;
    fn AXUIElementIsAttributeSettable(element: Ref, attribute: Ref, settable: *mut bool) -> i32;
    fn AXValueGetValue(value: Ref, kind: u32, output: *mut c_void) -> bool;
    fn CGEventCreateKeyboardEvent(source: Ref, key: u16, down: bool) -> Ref;
    fn CGEventKeyboardSetUnicodeString(event: Ref, length: usize, string: *const u16);
    fn CGEventSetFlags(event: Ref, flags: u64);
    fn CGEventPostToPid(pid: i32, event: Ref);
    fn CGEventSourceFlagsState(state: i32) -> u64;
}
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(value: Ref);
    fn CFEqual(a: Ref, b: Ref) -> bool;
    fn CFGetTypeID(value: Ref) -> usize;
    fn CFStringGetTypeID() -> usize;
    fn CFStringCreateWithCString(allocator: Ref, value: *const c_char, encoding: u32) -> Ref;
    fn CFStringGetLength(value: Ref) -> isize;
    fn CFStringGetMaximumSizeForEncoding(length: isize, encoding: u32) -> isize;
    fn CFStringGetCString(value: Ref, buffer: *mut c_char, size: isize, encoding: u32) -> bool;
    fn CFDictionaryCreate(
        allocator: Ref,
        keys: *const Ref,
        values: *const Ref,
        count: isize,
        key_callbacks: Ref,
        value_callbacks: Ref,
    ) -> Ref;
    static kCFBooleanTrue: Ref;
    static kCFTypeDictionaryKeyCallBacks: c_void;
    static kCFTypeDictionaryValueCallBacks: c_void;
}
#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn IsSecureEventInputEnabled() -> bool;
}
#[link(name = "proc")]
unsafe extern "C" {
    fn proc_pidpath(pid: i32, buffer: *mut c_void, size: u32) -> i32;
}
unsafe extern "C" {
    fn kill(pid: i32, signal: i32) -> i32;
    fn getuid() -> u32;
}
struct Cf(Ref);
impl Cf {
    fn new(value: Ref) -> Result<Self, String> {
        if value.is_null() {
            Err("macOS returned a null accessibility object.".into())
        } else {
            Ok(Self(value))
        }
    }
    fn string(value: &str) -> Result<Self, String> {
        let value = std::ffi::CString::new(value).map_err(|e| e.to_string())?;
        unsafe {
            Self::new(CFStringCreateWithCString(
                std::ptr::null(),
                value.as_ptr(),
                0x0800_0100,
            ))
        }
    }
    fn attribute(&self, name: &str) -> Result<Self, String> {
        let attribute = Self::string(name)?;
        let mut value = std::ptr::null();
        let code = unsafe { AXUIElementCopyAttributeValue(self.0, attribute.0, &mut value) };
        if code != 0 {
            return Err(format!(
                "Accessibility attribute {name} unavailable ({code})."
            ));
        }
        Self::new(value)
    }
    fn text(&self) -> Result<String, String> {
        unsafe {
            if CFGetTypeID(self.0) != CFStringGetTypeID() {
                return Err("Accessibility value is not text.".into());
            }
            let size =
                CFStringGetMaximumSizeForEncoding(CFStringGetLength(self.0), 0x0800_0100) + 1;
            if !(1..=1_048_577).contains(&size) {
                return Err("Destination text exceeds verification limit.".into());
            }
            let mut bytes = vec![0u8; size as usize];
            if !CFStringGetCString(self.0, bytes.as_mut_ptr().cast(), size, 0x0800_0100) {
                return Err("Could not read destination text.".into());
            }
            let length = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
            String::from_utf8(bytes[..length].to_vec()).map_err(|e| e.to_string())
        }
    }
    fn range(&self) -> Result<Range, String> {
        let value = self.attribute("AXSelectedTextRange")?;
        let mut range = Range {
            location: 0,
            length: 0,
        };
        if !unsafe { AXValueGetValue(value.0, 4, (&mut range as *mut Range).cast()) }
            || range.location < 0
            || range.length != 0
        {
            return Err("Destination selection is not an empty insertion cursor.".into());
        }
        Ok(range)
    }
}
impl Drop for Cf {
    fn drop(&mut self) {
        unsafe {
            CFRelease(self.0);
        }
    }
}

pub struct Destination {
    system: Cf,
    element: Cf,
    pid: i32,
    terminal: bool,
    expected: Option<String>,
    caret: Option<Range>,
    failed: bool,
}
impl Destination {
    pub fn capture(allow_terminal: bool) -> Result<Self, String> {
        if !unsafe { AXIsProcessTrusted() } {
            return Err("Grant Accessibility permission in Settings → Desktop typing.".into());
        }
        if unsafe { IsSecureEventInputEnabled() } {
            return Err("Secure keyboard input is enabled; desktop typing is paused.".into());
        }
        let system = Cf::new(unsafe { AXUIElementCreateSystemWide() })?;
        unsafe {
            AXUIElementSetMessagingTimeout(system.0, 0.3);
        }
        let element = system.attribute("AXFocusedUIElement")?;
        unsafe {
            AXUIElementSetMessagingTimeout(element.0, 0.3);
        }
        let mut pid = 0;
        if unsafe { AXUIElementGetPid(element.0, &mut pid) } != 0 || pid <= 0 {
            return Err("Focused application has no valid process identity.".into());
        }
        if pid as u32 == std::process::id() {
            return Err("Click an external text field to bind desktop typing.".into());
        }
        // kill(pid, 0) performs an ownership/permission check, not a signal.
        if unsafe { getuid() == 0 || kill(pid, 0) != 0 } {
            return Err("Privileged or foreign-user destinations are refused.".into());
        }
        let role = element.attribute("AXRole")?.text()?;
        let subrole = element
            .attribute("AXSubrole")
            .and_then(|v| v.text())
            .unwrap_or_default();
        if subrole.to_ascii_lowercase().contains("secure")
            || !matches!(role.as_str(), "AXTextField" | "AXTextArea" | "AXComboBox")
        {
            return Err("Focused object is not a verified non-password text field.".into());
        }
        let mut path = [0u8; 4096];
        let length = unsafe { proc_pidpath(pid, path.as_mut_ptr().cast(), path.len() as u32) };
        if length <= 0 {
            return Err("Destination process could not be verified.".into());
        }
        let end = path.iter().position(|b| *b == 0).unwrap_or(path.len());
        let path = String::from_utf8_lossy(&path[..end]);
        let name = path.rsplit('/').next().unwrap_or("").to_ascii_lowercase();
        let terminal = [
            "terminal",
            "iterm2",
            "iterm",
            "alacritty",
            "wezterm-gui",
            "kitty",
            "ghostty",
        ]
        .contains(&name.as_str());
        if terminal && !allow_terminal {
            return Err("Enable terminal typing in Settings first.".into());
        }
        let mut settable = false;
        let attribute = Cf::string("AXSelectedText")?;
        let writable =
            unsafe { AXUIElementIsAttributeSettable(element.0, attribute.0, &mut settable) } == 0
                && settable;
        if !terminal && !writable {
            return Err("Focused field does not expose a writable text cursor.".into());
        }
        let (expected, caret) = if terminal {
            (None, None)
        } else {
            (
                Some(element.attribute("AXValue")?.text()?),
                Some(element.range()?),
            )
        };
        Ok(Self {
            system,
            element,
            pid,
            terminal,
            expected,
            caret,
            failed: false,
        })
    }
    pub fn validate(&self) -> Result<(), String> {
        self.check_focus()?;
        if !self.terminal
            && (self.expected.as_deref()
                != Some(self.element.attribute("AXValue")?.text()?.as_str())
                || self.caret != Some(self.element.range()?))
        {
            return Err("Destination text or cursor changed outside dictation.".into());
        }
        Ok(())
    }

    fn check_focus(&self) -> Result<(), String> {
        if self.failed || unsafe { IsSecureEventInputEnabled() } {
            return Err("Secure input enabled or desktop typing already stopped.".into());
        }
        let focused = self.system.attribute("AXFocusedUIElement")?;
        let mut pid = 0;
        if unsafe { AXUIElementGetPid(focused.0, &mut pid) } != 0
            || pid != self.pid
            || !unsafe { CFEqual(focused.0, self.element.0) }
        {
            return Err("Destination focus changed.".into());
        }
        let subrole = focused
            .attribute("AXSubrole")
            .and_then(|v| v.text())
            .unwrap_or_default();
        if subrole.to_ascii_lowercase().contains("secure") {
            return Err("Destination became a password field.".into());
        }
        Ok(())
    }
    pub fn insert(&mut self, value: &str, cancel: &AtomicBool) -> Result<(), String> {
        let result = self.insert_inner(value, cancel);
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    fn insert_inner(&mut self, value: &str, cancel: &AtomicBool) -> Result<(), String> {
        self.validate()?;
        let text = crate::desktop::keyboard_text(value);
        for c in text.chars() {
            if cancel.load(Ordering::Acquire) {
                return Err("Dictation cancelled; already typed text is retained.".into());
            }
            self.check_focus()?;
            if modifiers_down() {
                return Err("Keyboard modifiers changed during typing.".into());
            }
            let mut buffer = [0u16; 2];
            let units = c.encode_utf16(&mut buffer);
            // Create both events before posting, so allocation failure cannot
            // leave a synthetic key down. CGEventPostToPid does not steal focus.
            let down = Cf::new(unsafe { CGEventCreateKeyboardEvent(std::ptr::null(), 0, true) })?;
            let up = Cf::new(unsafe { CGEventCreateKeyboardEvent(std::ptr::null(), 0, false) })?;
            unsafe {
                CGEventSetFlags(down.0, 0);
                CGEventSetFlags(up.0, 0);
                CGEventKeyboardSetUnicodeString(down.0, units.len(), units.as_ptr());
                CGEventKeyboardSetUnicodeString(up.0, units.len(), units.as_ptr());
                CGEventPostToPid(self.pid, down.0);
                CGEventPostToPid(self.pid, up.0);
            }
        }
        if let Some(before) = &self.expected {
            let deadline = Instant::now() + Duration::from_millis(500);
            loop {
                self.check_focus()?;
                let after = self.element.attribute("AXValue")?.text()?;
                if crate::desktop::is_insertion(before, &after, &text) {
                    self.expected = Some(after);
                    self.caret = Some(self.element.range()?);
                    break;
                }
                if Instant::now() >= deadline {
                    return Err(
                        "macOS text insertion was not acknowledged; no retry will be sent.".into(),
                    );
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        Ok(())
    }
}
pub fn modifiers_down() -> bool {
    unsafe { CGEventSourceFlagsState(1) & ((1 << 17) | (1 << 18) | (1 << 19) | (1 << 20)) != 0 }
}
pub fn status() -> super::DesktopStatus {
    let ready = unsafe { AXIsProcessTrusted() };
    super::DesktopStatus {
        supported: true,
        keyboard_ready: ready,
        permission_required: !ready,
        platform: "macOS",
        message: if ready {
            "Accessibility granted. Real macOS application testing is still required.".into()
        } else {
            "Grant Accessibility permission to Arxell in System Settings → Privacy & Security."
                .into()
        },
    }
}
pub fn prepare() -> Result<(), String> {
    unsafe {
        let keys = [kAXTrustedCheckOptionPrompt];
        let values = [kCFBooleanTrue];
        let options = Cf::new(CFDictionaryCreate(
            std::ptr::null(),
            keys.as_ptr(),
            values.as_ptr(),
            1,
            &raw const kCFTypeDictionaryKeyCallBacks,
            &raw const kCFTypeDictionaryValueCallBacks,
        ))?;
        if AXIsProcessTrustedWithOptions(options.0) {
            Ok(())
        } else {
            Err("Approve Arxell under System Settings → Privacy & Security → Accessibility, then refresh desktop status.".into())
        }
    }
}
